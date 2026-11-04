//! Validated, value-only plan editing for synthetic/general inventory.
//!
//! This crate has no filesystem, process, database, approval, or execution API.
//! Its projections and frozen reviews are permanently non-executable.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use disk_steward_core::{
    DuplicateGroupId, EvidenceId, GroupId, InventoryItemId, PlanId, ReviewId, RevisionId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ITEMS: usize = 10_000;
pub const MAX_GROUPS: usize = 1_000;
pub const MAX_TREE_DEPTH: usize = 64;
pub const MAX_GROUP_MEMBERSHIPS: usize = 20_000;
pub const MAX_LABEL_BYTES: usize = 512;
pub const MAX_BLOCKER_BYTES: usize = 2_048;
pub const MAX_EVIDENCE_PER_ITEM: usize = 128;
pub const MAX_BLOCKERS_PER_ITEM: usize = 32;
pub const MAX_TOTAL_EVIDENCE: usize = 50_000;
pub const MAX_TOTAL_BLOCKERS: usize = 50_000;
pub const MAX_HISTORY: usize = 64;
pub const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SERIALIZED_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_STATE_BYTES: usize = 24 * 1024 * 1024;
const MAX_DUPLICATE_EVIDENCE_ITEMS: usize = 32;
const MAX_DUPLICATE_EVIDENCE_SETS: usize = 16;
const MAX_DUPLICATE_EVIDENCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PYTEST_EVIDENCE_ITEMS: usize = 2;
const MAX_PYTEST_EVIDENCE_FILE_BYTES: u64 = 64 * 1024;
const REVIEW_VERSION: &str = "disk-steward-general-review-v1";

/// Builds an isolated synthetic inventory. It never resolves or reads a path.
pub fn synthetic_fixture(
    item_count: usize,
    group_count: usize,
) -> Result<PlanInput, ValidationError> {
    if item_count > MAX_ITEMS {
        return Err(ValidationError::TooManyItems);
    }
    if group_count > MAX_GROUPS {
        return Err(ValidationError::TooManyGroups);
    }
    if item_count > 0 && group_count == 0 {
        return Err(ValidationError::InvalidGroup);
    }
    let plan_id = PlanId::new();
    if item_count == 0 {
        return Ok(PlanInput {
            plan_id,
            items: Vec::new(),
            groups: Vec::new(),
            duplicate_sets: Vec::new(),
        });
    }
    if group_count == 0 || item_count <= group_count * 2 {
        return Err(ValidationError::InvalidGroup);
    }
    let file_count = item_count - group_count;
    let base_files = file_count / group_count;
    let extra_files = file_count % group_count;
    if base_files < 8 {
        return Err(ValidationError::InvalidGroup);
    }
    let mut items = Vec::with_capacity(item_count);
    let mut groups = Vec::with_capacity(group_count);
    let mut duplicate_pairs: Vec<Vec<InventoryItemId>> = Vec::new();
    for group_index in 0..group_count {
        let label = match group_index % 4 {
            0 => format!("Example project {:02}", group_index + 1),
            1 => format!("Example cache {:02}", group_index + 1),
            2 => format!("Example downloads {:02}", group_index + 1),
            _ => format!("Example workspace {:02}", group_index + 1),
        };
        let folder_id = InventoryItemId::new();
        let child_count = base_files + usize::from(group_index < extra_files);
        let mut child_ids = Vec::with_capacity(child_count);
        let mut child_items = Vec::with_capacity(child_count);
        let mut known_logical = 0_u64;
        let mut lower = 0_u64;
        let mut upper = 0_u64;
        let mut unknown = false;
        for child_index in 0..child_count {
            let id = InventoryItemId::new();
            let is_dup = child_index == 4 || child_index == 5;
            let size = if is_dup {
                Some(4 * 1024 * 1024)
            } else if child_index == 3 {
                None
            } else if child_index == 6 || child_index == 7 {
                Some(16_384)
            } else {
                Some(8_192 + ((group_index * 97 + child_index * 37) as u64 * 1024))
            };
            let (name, choice, blocker) = match child_index {
                0 => ("valuable-project-notes.md".to_owned(), Choice::Keep, None),
                1 => (
                    "generated-cache-entry.bin".to_owned(),
                    Choice::Include,
                    None,
                ),
                2 => (
                    "managed-library-record.dat".to_owned(),
                    Choice::Include,
                    Some(Blocker {
                        code: "synthetic_managed_data".to_owned(),
                        reason: "Synthetic managed data is blocked".to_owned(),
                    }),
                ),
                3 => (
                    "unclassified-installer.pkg".to_owned(),
                    Choice::Investigate,
                    None,
                ),
                4 => (
                    format!("duplicate-copy-a-{}.bin", group_index + 1),
                    Choice::Include,
                    None,
                ),
                5 => (
                    format!("duplicate-copy-b-{}.bin", group_index + 1),
                    Choice::Include,
                    None,
                ),
                6 | 7 => (
                    format!(
                        "shared-storage-alias-{}-{}.dat",
                        group_index + 1,
                        child_index - 5
                    ),
                    Choice::Include,
                    None,
                ),
                _ => (
                    format!(
                        "synthetic-finding-{}-{}.bin",
                        group_index + 1,
                        child_index + 1
                    ),
                    if child_index % 11 == 0 {
                        Choice::Investigate
                    } else {
                        Choice::Include
                    },
                    None,
                ),
            };
            let evidence = if is_dup {
                vec![EvidenceRef {
                    id: EvidenceId::new(),
                    summary: format!(
                        "Synthetic verified matching content fingerprint for duplicate pair {}",
                        group_index + 1
                    ),
                }]
            } else if child_index < 4 {
                vec![EvidenceRef {
                    id: EvidenceId::new(),
                    summary: "Synthetic fixture evidence; no user file was read".to_owned(),
                }]
            } else {
                Vec::new()
            };
            if let Some(value) = size {
                known_logical = known_logical.saturating_add(value);
                lower = lower.saturating_add(value / 2);
                upper = upper.saturating_add(value);
            } else {
                unknown = true;
            }
            child_ids.push(id);
            child_items.push(InventoryItem {
                id,
                parent_id: Some(folder_id),
                label: name,
                kind: ItemKind::File,
                logical_bytes: size,
                recovery_lower_bytes: size.map(|v| v / 2),
                recovery_upper_bytes: size,
                storage_identity: if child_index == 6 || child_index == 7 {
                    Some(format!("synthetic-shared-storage-{}", group_index + 1))
                } else {
                    None
                },
                blockers: blocker.into_iter().collect(),
                evidence,
                suggestion: Suggestion {
                    choice,
                    rationale: "Synthetic review suggestion".to_owned(),
                },
            });
        }
        if group_index < 3 {
            duplicate_pairs.push(vec![child_ids[4], child_ids[5]]);
        }
        let mut member_ids = vec![folder_id];
        member_ids.extend(child_ids);
        groups.push(PlanGroup {
            id: GroupId::new(),
            label: label.clone(),
            item_ids: member_ids,
        });
        items.push(InventoryItem {
            id: folder_id,
            parent_id: None,
            label: format!("{} folder", label),
            kind: ItemKind::Folder,
            logical_bytes: (!unknown).then_some(known_logical),
            recovery_lower_bytes: (!unknown).then_some(lower),
            recovery_upper_bytes: (!unknown).then_some(upper),
            storage_identity: None,
            blockers: Vec::new(),
            evidence: vec![EvidenceRef {
                id: EvidenceId::new(),
                summary: "Synthetic folder summary with mixed child types".to_owned(),
            }],
            suggestion: Suggestion {
                choice: Choice::Investigate,
                rationale: "Synthetic grouped findings".to_owned(),
            },
        });
        items.extend(child_items);
    }
    let duplicate_sets = duplicate_pairs
        .into_iter()
        .enumerate()
        .map(|(index, item_ids)| {
            let survivor_id = (index != 0).then_some(item_ids[0]);
            DuplicateSet {
                id: DuplicateGroupId::new(),
                item_ids,
                survivor_id,
            }
        })
        .collect();
    Ok(PlanInput {
        plan_id,
        items,
        groups,
        duplicate_sets,
    })
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    File,
    Folder,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    Keep,
    Include,
    Investigate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Suggestion {
    pub choice: Choice,
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Blocker {
    pub code: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InventoryItem {
    pub id: InventoryItemId,
    pub parent_id: Option<InventoryItemId>,
    pub label: String,
    pub kind: ItemKind,
    pub logical_bytes: Option<u64>,
    pub recovery_lower_bytes: Option<u64>,
    pub recovery_upper_bytes: Option<u64>,
    /// Identity shared by hard links/clones/other known shared storage.
    pub storage_identity: Option<String>,
    pub blockers: Vec<Blocker>,
    pub evidence: Vec<EvidenceRef>,
    pub suggestion: Suggestion,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EvidenceRef {
    pub id: EvidenceId,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanGroup {
    pub id: GroupId,
    pub label: String,
    pub item_ids: Vec<InventoryItemId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DuplicateSet {
    pub id: DuplicateGroupId,
    pub item_ids: Vec<InventoryItemId>,
    pub survivor_id: Option<InventoryItemId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanInput {
    pub plan_id: PlanId,
    pub items: Vec<InventoryItem>,
    pub groups: Vec<PlanGroup>,
    pub duplicate_sets: Vec<DuplicateSet>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InventoryRefresh {
    pub items: Vec<InventoryItem>,
    pub groups: Vec<PlanGroup>,
    pub duplicate_sets: Vec<DuplicateSet>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PlanEdit {
    SetChoice {
        item_id: InventoryItemId,
        choice: Choice,
    },
    SetMembership {
        item_id: InventoryItemId,
        in_plan: bool,
    },
    SetGroupChoice {
        group_id: GroupId,
        choice: Choice,
    },
    ExcludeDescendants {
        parent_id: InventoryItemId,
        item_ids: Vec<InventoryItemId>,
    },
    SetDuplicateSurvivor {
        duplicate_set_id: DuplicateGroupId,
        survivor_id: InventoryItemId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValidationError {
    TooManyItems,
    TooManyGroups,
    DuplicateIdentity,
    MissingReference,
    InvalidTree,
    InvalidGroup,
    InvalidDuplicateSet,
    InvalidBounds,
    InvalidLabel,
    InvalidBlocker,
    TooManyEvidence,
    SerializedStateTooLarge,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EditError {
    StaleRevision,
    UnknownItem,
    UnknownGroup,
    UnknownDuplicateSet,
    InvalidExclusion,
    InvalidSurvivor,
    SerializedStateTooLarge,
    InvalidInventory,
    EmptyHistory,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionKind {
    NonExecutableEditor,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Estimate {
    pub selected_count: usize,
    pub logical_selected_bytes: u64,
    pub in_plan_logical_bytes: u64,
    pub recovery_lower_bytes: u64,
    pub recovery_upper_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ItemProjection {
    pub item_id: InventoryItemId,
    pub parent_id: Option<InventoryItemId>,
    pub label: String,
    pub kind: ItemKind,
    pub logical_bytes: Option<u64>,
    pub suggestion: Suggestion,
    pub user_choice: Option<Choice>,
    pub in_plan: bool,
    pub effective_selected: bool,
    pub blockers: Vec<Blocker>,
    pub evidence: Vec<EvidenceRef>,
    pub excluded_descendant_ids: Vec<InventoryItemId>,
    pub group_ids: Vec<GroupId>,
    pub recovery_lower_bytes: Option<u64>,
    pub recovery_upper_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GroupProjection {
    pub id: GroupId,
    pub label: String,
    pub member_ids: Vec<InventoryItemId>,
    pub selected_count: usize,
    pub selected_logical_bytes: u64,
    pub blocked_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DuplicateProjection {
    pub id: DuplicateGroupId,
    pub member_ids: Vec<InventoryItemId>,
    pub survivor_id: Option<InventoryItemId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EditorSnapshot {
    pub schema_version: u32,
    pub kind: ProjectionKind,
    pub execution_available: bool,
    pub plan_id: PlanId,
    pub revision_id: RevisionId,
    pub revision_number: u64,
    pub items: Vec<ItemProjection>,
    pub groups: Vec<GroupProjection>,
    pub duplicates: Vec<DuplicateProjection>,
    pub totals: Estimate,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FrozenReview {
    pub schema_version: u32,
    pub kind: ProjectionKind,
    pub execution_available: bool,
    pub review_id: ReviewId,
    pub plan_id: PlanId,
    pub revision_id: RevisionId,
    pub revision_number: u64,
    pub review_version: String,
    pub items: Vec<ItemProjection>,
    pub groups: Vec<GroupProjection>,
    pub duplicates: Vec<DuplicateProjection>,
    pub totals: Estimate,
    pub content_hash: String,
}

impl FrozenReview {
    /// Checks the fixed review contract and its deterministic content digest.
    #[must_use]
    pub fn verify_content_hash(&self) -> bool {
        self.schema_version == 1
            && self.review_version == REVIEW_VERSION
            && self.kind == ProjectionKind::NonExecutableEditor
            && !self.execution_available
            && self.content_hash == review_manifest_hash(&ReviewManifest::from(self))
    }
}

#[derive(Serialize)]
struct ReviewManifest<'a> {
    review_version: &'static str,
    schema_version: u32,
    kind: &'a ProjectionKind,
    execution_available: bool,
    plan_id: PlanId,
    revision_id: RevisionId,
    revision_number: u64,
    items: &'a [ItemProjection],
    groups: &'a [GroupProjection],
    duplicates: &'a [DuplicateProjection],
    totals: &'a Estimate,
}

impl<'a> From<&'a FrozenReview> for ReviewManifest<'a> {
    fn from(review: &'a FrozenReview) -> Self {
        Self {
            review_version: REVIEW_VERSION,
            schema_version: review.schema_version,
            kind: &review.kind,
            execution_available: review.execution_available,
            plan_id: review.plan_id,
            revision_id: review.revision_id,
            revision_number: review.revision_number,
            items: &review.items,
            groups: &review.groups,
            duplicates: &review.duplicates,
            totals: &review.totals,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EditOutcome {
    pub snapshot: EditorSnapshot,
    /// Blocked members requested by a group Include edit, with explicit reason.
    pub blocked: Vec<BlockedMember>,
    pub changed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BlockedMember {
    pub item_id: InventoryItemId,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct State {
    items: Vec<InventoryItem>,
    groups: Vec<PlanGroup>,
    duplicate_sets: Vec<DuplicateSet>,
    choices: BTreeMap<InventoryItemId, Choice>,
    membership: BTreeMap<InventoryItemId, bool>,
    exclusions: BTreeMap<InventoryItemId, BTreeSet<InventoryItemId>>,
}

#[derive(Clone, Debug)]
pub struct PlanEditor {
    plan_id: PlanId,
    revision_id: RevisionId,
    revision_number: u64,
    state: State,
    undo: VecDeque<State>,
    redo: VecDeque<State>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    version: u32,
    plan_id: PlanId,
    revision_id: RevisionId,
    revision_number: u64,
    state: State,
    undo: Vec<State>,
    redo: Vec<State>,
}

impl PlanEditor {
    pub fn new(input: PlanInput) -> Result<Self, ValidationError> {
        validate(&input.items, &input.groups, &input.duplicate_sets)?;
        let mut items = input.items;
        let mut groups = input.groups;
        let mut duplicate_sets = input.duplicate_sets;
        items.sort_by_key(|i| i.id);
        groups.sort_by_key(|g| g.id);
        duplicate_sets.sort_by_key(|d| d.id);
        let state = State {
            membership: items.iter().map(|i| (i.id, true)).collect(),
            items,
            groups,
            duplicate_sets,
            choices: BTreeMap::new(),
            exclusions: BTreeMap::new(),
        };
        check_state_size(&state)?;
        Ok(Self {
            plan_id: input.plan_id,
            revision_id: RevisionId::new(),
            revision_number: 1,
            state,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> EditorSnapshot {
        project(
            self.plan_id,
            self.revision_id,
            self.revision_number,
            &self.state,
            !self.undo.is_empty(),
            !self.redo.is_empty(),
        )
    }

    /// Exports the bounded, versioned authoritative domain archive for a persistence layer.
    pub fn export_document_json(&self) -> Result<Vec<u8>, ValidationError> {
        let archive = Archive {
            version: 1,
            plan_id: self.plan_id,
            revision_id: self.revision_id,
            revision_number: self.revision_number,
            state: self.state.clone(),
            undo: self.undo.iter().cloned().collect(),
            redo: self.redo.iter().cloned().collect(),
        };
        let bytes =
            serde_json::to_vec(&archive).map_err(|_| ValidationError::SerializedStateTooLarge)?;
        if bytes.len() > MAX_SERIALIZED_BYTES {
            return Err(ValidationError::SerializedStateTooLarge);
        }
        Ok(bytes)
    }

    /// Imports only the explicit versioned archive shape, validating every state/history entry.
    pub fn import_document_json(bytes: &[u8]) -> Result<Self, ValidationError> {
        if bytes.len() > MAX_SERIALIZED_BYTES {
            return Err(ValidationError::SerializedStateTooLarge);
        }
        let archive: Archive =
            serde_json::from_slice(bytes).map_err(|_| ValidationError::SerializedStateTooLarge)?;
        if archive.version != 1
            || archive.revision_number == 0
            || archive.undo.len() > MAX_HISTORY
            || archive.redo.len() > MAX_HISTORY
        {
            return Err(ValidationError::SerializedStateTooLarge);
        }
        validate_state(&archive.state)?;
        for state in archive.undo.iter().chain(archive.redo.iter()) {
            validate_state(state)?;
        }
        let undo: VecDeque<_> = archive.undo.into();
        let redo: VecDeque<_> = archive.redo.into();
        if history_size(&undo) + history_size(&redo) > MAX_HISTORY_BYTES {
            return Err(ValidationError::SerializedStateTooLarge);
        }
        Ok(Self {
            plan_id: archive.plan_id,
            revision_id: archive.revision_id,
            revision_number: archive.revision_number,
            state: archive.state,
            undo,
            redo,
        })
    }

    pub fn edit(
        &mut self,
        expected_revision: RevisionId,
        edit: PlanEdit,
    ) -> Result<EditOutcome, EditError> {
        if expected_revision != self.revision_id {
            return Err(EditError::StaleRevision);
        }
        let mut next = self.state.clone();
        let mut blocked = Vec::new();
        apply_edit(&mut next, edit, &mut blocked)?;
        if next == self.state {
            return Ok(EditOutcome {
                snapshot: self.snapshot(),
                blocked,
                changed: false,
            });
        }
        check_state_size(&next).map_err(|_| EditError::SerializedStateTooLarge)?;
        push_bounded(&mut self.undo, self.state.clone());
        self.redo.clear();
        trim_history(&mut self.undo, &mut self.redo);
        self.state = next;
        self.revision_id = RevisionId::new();
        self.revision_number = self.revision_number.saturating_add(1);
        Ok(EditOutcome {
            snapshot: self.snapshot(),
            blocked,
            changed: true,
        })
    }

    pub fn undo(&mut self, expected_revision: RevisionId) -> Result<EditorSnapshot, EditError> {
        self.check_revision(expected_revision)?;
        let prior = self.undo.pop_back().ok_or(EditError::EmptyHistory)?;
        push_bounded(&mut self.redo, self.state.clone());
        trim_history(&mut self.undo, &mut self.redo);
        self.state = prior;
        self.revision_id = RevisionId::new();
        self.revision_number = self.revision_number.saturating_add(1);
        Ok(self.snapshot())
    }

    pub fn redo(&mut self, expected_revision: RevisionId) -> Result<EditorSnapshot, EditError> {
        self.check_revision(expected_revision)?;
        let next = self.redo.pop_back().ok_or(EditError::EmptyHistory)?;
        push_bounded(&mut self.undo, self.state.clone());
        trim_history(&mut self.undo, &mut self.redo);
        self.state = next;
        self.revision_id = RevisionId::new();
        self.revision_number = self.revision_number.saturating_add(1);
        Ok(self.snapshot())
    }

    pub fn refresh(
        &mut self,
        expected_revision: RevisionId,
        refresh: InventoryRefresh,
    ) -> Result<EditorSnapshot, EditError> {
        self.check_revision(expected_revision)?;
        validate(&refresh.items, &refresh.groups, &refresh.duplicate_sets)
            .map_err(|_| EditError::InvalidInventory)?;
        let mut items = refresh.items;
        items.sort_by_key(|i| i.id);
        let ids: BTreeSet<_> = items.iter().map(|i| i.id).collect();
        let mut groups = refresh.groups;
        groups.sort_by_key(|g| g.id);
        let mut duplicate_sets = refresh.duplicate_sets;
        duplicate_sets.sort_by_key(|d| d.id);
        let old_duplicate_by_id: BTreeMap<_, _> = self
            .state
            .duplicate_sets
            .iter()
            .map(|set| (set.id, set))
            .collect();
        for d in &mut duplicate_sets {
            if let Some(old) = old_duplicate_by_id.get(&d.id) {
                if old.survivor_id.is_some_and(|s| d.item_ids.contains(&s)) {
                    d.survivor_id = old.survivor_id;
                }
            }
        }
        let mut next = State {
            membership: items
                .iter()
                .map(|i| {
                    (
                        i.id,
                        self.state.membership.get(&i.id).copied().unwrap_or(true),
                    )
                })
                .collect(),
            choices: self
                .state
                .choices
                .iter()
                .filter(|(id, _)| ids.contains(id))
                .map(|(id, c)| (*id, *c))
                .collect(),
            exclusions: BTreeMap::new(),
            items,
            groups,
            duplicate_sets,
        };
        let next_parents: BTreeMap<_, _> = next
            .items
            .iter()
            .map(|item| (item.id, item.parent_id))
            .collect();
        let folder_ids: BTreeSet<_> = next
            .items
            .iter()
            .filter(|item| item.kind == ItemKind::Folder)
            .map(|item| item.id)
            .collect();
        next.exclusions = self
            .state
            .exclusions
            .iter()
            .filter_map(|(parent, old_excluded)| {
                if !folder_ids.contains(parent) {
                    return None;
                }
                let retained: BTreeSet<_> = old_excluded
                    .iter()
                    .filter(|child| {
                        ids.contains(child)
                            && is_descendant_with_parents(&next_parents, **child, *parent)
                    })
                    .copied()
                    .collect();
                if retained.is_empty() {
                    None
                } else {
                    Some((*parent, retained))
                }
            })
            .collect();
        check_state_size(&next).map_err(|_| EditError::SerializedStateTooLarge)?;
        if next == self.state {
            return Ok(self.snapshot());
        }
        push_bounded(&mut self.undo, self.state.clone());
        self.redo.clear();
        self.state = next;
        self.revision_id = RevisionId::new();
        self.revision_number = self.revision_number.saturating_add(1);
        Ok(self.snapshot())
    }

    /// Appends trusted inventory metadata while preserving all existing inventory
    /// and user state. This is a domain operation for native-owned callers; it is
    /// deliberately not representable as a renderer-submitted `PlanEdit`.
    pub fn add_items(
        &mut self,
        expected_revision: RevisionId,
        items_to_add: Vec<InventoryItem>,
    ) -> Result<EditorSnapshot, EditError> {
        self.check_revision(expected_revision)?;
        if items_to_add.is_empty() {
            return Ok(self.snapshot());
        }
        let mut items = self.state.items.clone();
        items.extend(items_to_add);
        self.refresh(
            expected_revision,
            InventoryRefresh {
                items,
                groups: self.state.groups.clone(),
                duplicate_sets: self.state.duplicate_sets.clone(),
            },
        )
    }

    /// Imports native-verified, read-only duplicate evidence without replacing
    /// existing inventory metadata, user choices, membership, or survivor picks.
    /// This operation is deliberately not representable as a renderer `PlanEdit`.
    pub fn add_duplicate_evidence(
        &mut self,
        expected_revision: RevisionId,
        incoming: Vec<InventoryItem>,
        sets: Vec<DuplicateSet>,
    ) -> Result<EditorSnapshot, EditError> {
        self.check_revision(expected_revision)?;
        if incoming.is_empty() && sets.is_empty() {
            return Ok(self.snapshot());
        }
        if incoming.is_empty()
            || sets.is_empty()
            || incoming.len() > MAX_DUPLICATE_EVIDENCE_ITEMS
            || sets.len() > MAX_DUPLICATE_EVIDENCE_SETS
        {
            return Err(EditError::InvalidInventory);
        }

        let mut incoming_ids = BTreeSet::new();
        for item in &incoming {
            if !incoming_ids.insert(item.id)
                || item.kind != ItemKind::File
                || item.parent_id.is_some()
                || !item
                    .logical_bytes
                    .is_some_and(|bytes| bytes <= MAX_DUPLICATE_EVIDENCE_FILE_BYTES)
                || item.suggestion.choice != Choice::Keep
                || item.storage_identity.is_some()
                || item.recovery_lower_bytes.is_some()
                || item.recovery_upper_bytes.is_some()
                || !item
                    .blockers
                    .iter()
                    .any(|blocker| blocker.code == "read_only_comparison")
            {
                return Err(EditError::InvalidInventory);
            }
            let mut evidence_ids = BTreeSet::new();
            if item
                .evidence
                .iter()
                .any(|evidence| !evidence_ids.insert(evidence.id))
            {
                return Err(EditError::InvalidInventory);
            }
        }

        let mut set_ids = BTreeSet::new();
        let mut covered_ids = BTreeSet::new();
        for set in &sets {
            if !set_ids.insert(set.id)
                || set.survivor_id.is_some()
                || set.item_ids.len() < 2
                || set
                    .item_ids
                    .iter()
                    .any(|id| !incoming_ids.contains(id) || !covered_ids.insert(*id))
            {
                return Err(EditError::InvalidInventory);
            }
        }
        if covered_ids != incoming_ids {
            return Err(EditError::InvalidInventory);
        }

        let mut next = self.state.clone();
        for incoming_item in incoming {
            if let Some(existing) = next
                .items
                .iter_mut()
                .find(|item| item.id == incoming_item.id)
            {
                if existing.kind != incoming_item.kind
                    || existing.parent_id != incoming_item.parent_id
                    || existing.label != incoming_item.label
                    || existing.logical_bytes != incoming_item.logical_bytes
                    || existing.storage_identity.is_some()
                    || existing.recovery_lower_bytes.is_some()
                    || existing.recovery_upper_bytes.is_some()
                {
                    return Err(EditError::InvalidInventory);
                }
                for incoming_evidence in incoming_item.evidence {
                    match existing
                        .evidence
                        .iter()
                        .find(|evidence| evidence.id == incoming_evidence.id)
                    {
                        Some(existing_evidence) if existing_evidence != &incoming_evidence => {
                            return Err(EditError::InvalidInventory);
                        }
                        Some(_) => {}
                        None => existing.evidence.push(incoming_evidence),
                    }
                }
                for blocker in incoming_item.blockers {
                    if !existing.blockers.contains(&blocker) {
                        existing.blockers.push(blocker);
                    }
                }
            } else {
                next.membership.insert(incoming_item.id, true);
                next.items.push(incoming_item);
            }
        }

        for incoming_set in sets {
            if let Some(existing) = next
                .duplicate_sets
                .iter()
                .find(|set| set.id == incoming_set.id)
            {
                let mut old_members = existing.item_ids.clone();
                let mut new_members = incoming_set.item_ids.clone();
                old_members.sort();
                new_members.sort();
                if old_members != new_members {
                    return Err(EditError::InvalidInventory);
                }
                // Preserve the established survivor and member ordering.
                continue;
            }
            let already_grouped: BTreeSet<_> = next
                .duplicate_sets
                .iter()
                .flat_map(|set| set.item_ids.iter().copied())
                .collect();
            if incoming_set
                .item_ids
                .iter()
                .any(|id| already_grouped.contains(id))
            {
                return Err(EditError::InvalidInventory);
            }
            next.duplicate_sets.push(incoming_set);
        }

        next.items.sort_by_key(|item| item.id);
        next.duplicate_sets.sort_by_key(|set| set.id);
        validate_state(&next).map_err(|error| match error {
            ValidationError::SerializedStateTooLarge => EditError::SerializedStateTooLarge,
            _ => EditError::InvalidInventory,
        })?;
        if next == self.state {
            return Ok(self.snapshot());
        }

        let mut candidate = self.clone();
        push_bounded(&mut candidate.undo, self.state.clone());
        candidate.redo.clear();
        candidate.state = next;
        candidate.revision_id = RevisionId::new();
        candidate.revision_number = candidate.revision_number.saturating_add(1);
        trim_history(&mut candidate.undo, &mut candidate.redo);
        candidate
            .export_document_json()
            .map_err(|_| EditError::SerializedStateTooLarge)?;
        *self = candidate;
        Ok(self.snapshot())
    }

    /// Additively imports native pytest-cache format evidence without changing
    /// existing choices, membership, exclusions, or duplicate-set state. This
    /// native-only operation is not representable as a renderer `PlanEdit`.
    pub fn add_pytest_evidence(
        &mut self,
        expected_revision: RevisionId,
        incoming: Vec<InventoryItem>,
    ) -> Result<EditorSnapshot, EditError> {
        self.check_revision(expected_revision)?;
        if incoming.is_empty() || incoming.len() > MAX_PYTEST_EVIDENCE_ITEMS {
            return Err(EditError::InvalidInventory);
        }

        let mut incoming_ids = BTreeSet::new();
        for item in &incoming {
            let mut evidence_ids = BTreeSet::new();
            if !incoming_ids.insert(item.id)
                || item.kind != ItemKind::File
                || item.parent_id.is_some()
                || !item
                    .logical_bytes
                    .is_some_and(|bytes| bytes <= MAX_PYTEST_EVIDENCE_FILE_BYTES)
                || item.suggestion.choice != Choice::Keep
                || item.storage_identity.is_some()
                || item.recovery_lower_bytes.is_some()
                || item.recovery_upper_bytes.is_some()
                || item.evidence.is_empty()
                || !item
                    .blockers
                    .iter()
                    .any(|blocker| blocker.code == "read_only_pytest")
                || item
                    .evidence
                    .iter()
                    .any(|evidence| !evidence_ids.insert(evidence.id))
            {
                return Err(EditError::InvalidInventory);
            }
        }

        let mut next = self.state.clone();
        for incoming_item in incoming {
            if let Some(existing) = next
                .items
                .iter_mut()
                .find(|item| item.id == incoming_item.id)
            {
                if existing.kind != incoming_item.kind
                    || existing.parent_id != incoming_item.parent_id
                    || existing.label != incoming_item.label
                    || existing.logical_bytes != incoming_item.logical_bytes
                    || existing.storage_identity.is_some()
                    || existing.recovery_lower_bytes.is_some()
                    || existing.recovery_upper_bytes.is_some()
                {
                    return Err(EditError::InvalidInventory);
                }
                for incoming_evidence in incoming_item.evidence {
                    match existing
                        .evidence
                        .iter()
                        .find(|evidence| evidence.id == incoming_evidence.id)
                    {
                        Some(existing_evidence) if existing_evidence != &incoming_evidence => {
                            return Err(EditError::InvalidInventory);
                        }
                        Some(_) => {}
                        None => existing.evidence.push(incoming_evidence),
                    }
                }
                for blocker in incoming_item.blockers {
                    if !existing.blockers.contains(&blocker) {
                        existing.blockers.push(blocker);
                    }
                }
            } else {
                next.membership.insert(incoming_item.id, true);
                next.items.push(incoming_item);
            }
        }

        next.items.sort_by_key(|item| item.id);
        validate_state(&next).map_err(|error| match error {
            ValidationError::SerializedStateTooLarge => EditError::SerializedStateTooLarge,
            _ => EditError::InvalidInventory,
        })?;
        if next == self.state {
            return Ok(self.snapshot());
        }

        let mut candidate = self.clone();
        push_bounded(&mut candidate.undo, self.state.clone());
        candidate.redo.clear();
        candidate.state = next;
        candidate.revision_id = RevisionId::new();
        candidate.revision_number = candidate.revision_number.saturating_add(1);
        trim_history(&mut candidate.undo, &mut candidate.redo);
        candidate
            .export_document_json()
            .map_err(|_| EditError::SerializedStateTooLarge)?;
        *self = candidate;
        Ok(self.snapshot())
    }

    pub fn freeze_review(&self, expected_revision: RevisionId) -> Result<FrozenReview, EditError> {
        self.check_revision(expected_revision)?;
        let snapshot = self.snapshot();
        let mut review = FrozenReview {
            kind: ProjectionKind::NonExecutableEditor,
            execution_available: false,
            review_id: ReviewId::new(),
            plan_id: self.plan_id,
            revision_id: self.revision_id,
            revision_number: self.revision_number,
            review_version: REVIEW_VERSION.to_owned(),
            items: snapshot.items,
            groups: snapshot.groups,
            duplicates: snapshot.duplicates,
            schema_version: 1,
            totals: snapshot.totals,
            content_hash: String::new(),
        };
        review.content_hash = review_manifest_hash(&ReviewManifest::from(&review));
        Ok(review)
    }

    fn check_revision(&self, expected: RevisionId) -> Result<(), EditError> {
        if expected == self.revision_id {
            Ok(())
        } else {
            Err(EditError::StaleRevision)
        }
    }
}

fn push_bounded(queue: &mut VecDeque<State>, value: State) {
    queue.push_back(value);
    while queue.len() > MAX_HISTORY || history_size(queue) > MAX_HISTORY_BYTES {
        queue.pop_front();
    }
}

fn history_size(queue: &VecDeque<State>) -> usize {
    queue
        .iter()
        .filter_map(|state| serde_json::to_vec(state).ok().map(|bytes| bytes.len()))
        .sum()
}

fn trim_history(undo: &mut VecDeque<State>, redo: &mut VecDeque<State>) {
    while undo.len() + redo.len() > MAX_HISTORY * 2
        || history_size(undo).saturating_add(history_size(redo)) > MAX_HISTORY_BYTES
    {
        if undo.is_empty() {
            redo.pop_front();
        } else if redo.is_empty() || history_size(undo) >= history_size(redo) {
            undo.pop_front();
        } else {
            redo.pop_front();
        }
    }
}

fn validate_state(state: &State) -> Result<(), ValidationError> {
    validate(&state.items, &state.groups, &state.duplicate_sets)?;
    let ids: BTreeSet<_> = state.items.iter().map(|item| item.id).collect();
    let items_by_id: BTreeMap<_, _> = state.items.iter().map(|item| (item.id, item)).collect();
    if state.membership.len() != ids.len()
        || state.membership.keys().any(|id| !ids.contains(id))
        || state.choices.keys().any(|id| !ids.contains(id))
    {
        return Err(ValidationError::MissingReference);
    }
    let parents: BTreeMap<_, _> = state
        .items
        .iter()
        .map(|item| (item.id, item.parent_id))
        .collect();
    for (parent, excluded) in &state.exclusions {
        let item = items_by_id
            .get(parent)
            .ok_or(ValidationError::MissingReference)?;
        if item.kind != ItemKind::Folder || excluded.is_empty() {
            return Err(ValidationError::InvalidTree);
        }
        if excluded
            .iter()
            .any(|id| !ids.contains(id) || !is_descendant_with_parents(&parents, *id, *parent))
        {
            return Err(ValidationError::InvalidTree);
        }
    }
    check_state_size(state)
}

fn apply_edit(
    state: &mut State,
    edit: PlanEdit,
    blocked: &mut Vec<BlockedMember>,
) -> Result<(), EditError> {
    let missing_survivor = missing_survivor_members(state);
    match edit {
        PlanEdit::SetChoice { item_id, choice } => {
            let item = state
                .items
                .iter()
                .find(|item| item.id == item_id)
                .ok_or(EditError::UnknownItem)?;
            let blockers = blockers_for_item(item, item_id, &missing_survivor);
            if choice == Choice::Include && !blockers.is_empty() {
                blocked.push(BlockedMember {
                    item_id,
                    reason: blockers
                        .iter()
                        .map(|b| b.reason.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                });
                return Ok(());
            }
            state.choices.insert(item_id, choice);
        }
        PlanEdit::SetMembership { item_id, in_plan } => {
            if !state.membership.contains_key(&item_id) {
                return Err(EditError::UnknownItem);
            }
            state.membership.insert(item_id, in_plan);
        }
        PlanEdit::SetGroupChoice { group_id, choice } => {
            let group = state
                .groups
                .iter()
                .find(|g| g.id == group_id)
                .ok_or(EditError::UnknownGroup)?;
            let items_by_id: BTreeMap<_, _> =
                state.items.iter().map(|item| (item.id, item)).collect();
            for id in &group.item_ids {
                let item = items_by_id.get(id).ok_or(EditError::UnknownItem)?;
                let blockers = blockers_for_item(item, *id, &missing_survivor);
                if choice == Choice::Include && state.choices.get(id) == Some(&Choice::Keep) {
                    blocked.push(BlockedMember {
                        item_id: *id,
                        reason: "kept_by_user".to_owned(),
                    });
                } else if choice == Choice::Include && !blockers.is_empty() {
                    blocked.push(BlockedMember {
                        item_id: *id,
                        reason: blockers
                            .iter()
                            .map(|b| b.reason.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                    });
                } else {
                    state.choices.insert(*id, choice);
                }
            }
        }
        PlanEdit::ExcludeDescendants {
            parent_id,
            item_ids,
        } => {
            let parent = state
                .items
                .iter()
                .find(|i| i.id == parent_id)
                .ok_or(EditError::UnknownItem)?;
            if parent.kind != ItemKind::Folder {
                return Err(EditError::InvalidExclusion);
            }
            let parents: BTreeMap<_, _> = state
                .items
                .iter()
                .map(|item| (item.id, item.parent_id))
                .collect();
            let mut proposed = BTreeSet::new();
            for id in item_ids {
                if !state.membership.contains_key(&id)
                    || !is_descendant_with_parents(&parents, id, parent_id)
                {
                    return Err(EditError::InvalidExclusion);
                }
                proposed.insert(id);
            }
            if proposed.is_empty() {
                state.exclusions.remove(&parent_id);
            } else {
                state.exclusions.insert(parent_id, proposed);
            }
        }
        PlanEdit::SetDuplicateSurvivor {
            duplicate_set_id,
            survivor_id,
        } => {
            let set = state
                .duplicate_sets
                .iter_mut()
                .find(|d| d.id == duplicate_set_id)
                .ok_or(EditError::UnknownDuplicateSet)?;
            if !set.item_ids.contains(&survivor_id) {
                return Err(EditError::InvalidSurvivor);
            }
            set.survivor_id = Some(survivor_id);
        }
    }
    Ok(())
}

fn missing_survivor_members(state: &State) -> BTreeSet<InventoryItemId> {
    state
        .duplicate_sets
        .iter()
        .filter(|set| set.survivor_id.is_none())
        .flat_map(|set| set.item_ids.iter().copied())
        .collect()
}

fn blockers_for_item(
    item: &InventoryItem,
    id: InventoryItemId,
    missing_survivor: &BTreeSet<InventoryItemId>,
) -> Vec<Blocker> {
    let mut blockers = item.blockers.clone();
    if missing_survivor.contains(&id) {
        blockers.push(Blocker {
            code: "duplicate_survivor_required".to_owned(),
            reason: "Choose which duplicate to keep before including this item".to_owned(),
        });
    }
    blockers
}

fn validate(
    items: &[InventoryItem],
    groups: &[PlanGroup],
    duplicate_sets: &[DuplicateSet],
) -> Result<(), ValidationError> {
    if items.len() > MAX_ITEMS {
        return Err(ValidationError::TooManyItems);
    }
    if groups.len() > MAX_GROUPS {
        return Err(ValidationError::TooManyGroups);
    }
    let mut ids = BTreeSet::new();
    let mut item_by_id = BTreeMap::new();
    let mut file_ids = BTreeSet::new();
    let mut parent_ids = BTreeSet::new();
    let mut total_evidence = 0_usize;
    let mut total_blockers = 0_usize;
    let mut variable_text_bytes = 0_usize;
    for item in items {
        if !ids.insert(item.id) {
            return Err(ValidationError::DuplicateIdentity);
        }
        item_by_id.insert(item.id, item);
        if item.kind == ItemKind::File {
            file_ids.insert(item.id);
        }
        if let Some(parent) = item.parent_id {
            parent_ids.insert(parent);
        }
        if item.label.trim().is_empty()
            || item.label.len() > MAX_LABEL_BYTES
            || item.label.contains('\0')
        {
            return Err(ValidationError::InvalidLabel);
        }
        if item.suggestion.rationale.len() > MAX_BLOCKER_BYTES {
            return Err(ValidationError::InvalidLabel);
        }
        variable_text_bytes = variable_text_bytes
            .saturating_add(item.label.len())
            .saturating_add(item.suggestion.rationale.len())
            .saturating_add(item.storage_identity.as_ref().map_or(0, String::len));
        if item
            .recovery_lower_bytes
            .zip(item.recovery_upper_bytes)
            .is_some_and(|(lo, hi)| lo > hi)
        {
            return Err(ValidationError::InvalidBounds);
        }
        if item.recovery_lower_bytes.is_some() != item.recovery_upper_bytes.is_some() {
            return Err(ValidationError::InvalidBounds);
        }
        if item
            .storage_identity
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > MAX_LABEL_BYTES)
        {
            return Err(ValidationError::InvalidLabel);
        }
        if item.evidence.len() > MAX_EVIDENCE_PER_ITEM {
            return Err(ValidationError::TooManyEvidence);
        }
        total_evidence = total_evidence.saturating_add(item.evidence.len());
        total_blockers = total_blockers.saturating_add(item.blockers.len());
        if item.blockers.len() > MAX_BLOCKERS_PER_ITEM {
            return Err(ValidationError::InvalidBlocker);
        }
        let mut evidence = BTreeSet::new();
        if item
            .evidence
            .iter()
            .any(|e| !evidence.insert(e.id) || e.summary.len() > MAX_BLOCKER_BYTES)
        {
            return Err(ValidationError::DuplicateIdentity);
        }
        for evidence in &item.evidence {
            variable_text_bytes = variable_text_bytes
                .saturating_add(evidence.summary.len())
                .saturating_add(96);
        }
        for blocker in &item.blockers {
            variable_text_bytes = variable_text_bytes
                .saturating_add(blocker.code.len())
                .saturating_add(blocker.reason.len())
                .saturating_add(64);
        }
        if item.blockers.iter().any(|b| {
            b.code.is_empty()
                || b.code.len() > 128
                || b.reason.is_empty()
                || b.reason.len() > MAX_BLOCKER_BYTES
        }) {
            return Err(ValidationError::InvalidBlocker);
        }
    }
    for item in items {
        if item.parent_id.is_some_and(|p| !ids.contains(&p)) {
            return Err(ValidationError::MissingReference);
        }
    }
    if file_ids.iter().any(|id| parent_ids.contains(id)) {
        return Err(ValidationError::InvalidTree);
    }
    if has_cycle_or_excess_depth(items) {
        return Err(ValidationError::InvalidTree);
    }
    let mut group_ids = BTreeSet::new();
    let mut membership_count = 0_usize;
    for group in groups {
        if !group_ids.insert(group.id) {
            return Err(ValidationError::DuplicateIdentity);
        }
        if group.label.trim().is_empty() || group.label.len() > MAX_LABEL_BYTES {
            return Err(ValidationError::InvalidLabel);
        }
        variable_text_bytes = variable_text_bytes.saturating_add(group.label.len());
        if group.item_ids.is_empty()
            || group.item_ids.len() > MAX_GROUP_MEMBERSHIPS
            || membership_count.saturating_add(group.item_ids.len()) > MAX_GROUP_MEMBERSHIPS
        {
            return Err(ValidationError::InvalidGroup);
        }
        let mut members = BTreeSet::new();
        if group
            .item_ids
            .iter()
            .any(|id| !ids.contains(id) || !members.insert(*id))
        {
            return Err(ValidationError::InvalidGroup);
        }
        membership_count += group.item_ids.len();
        if membership_count > MAX_GROUP_MEMBERSHIPS {
            return Err(ValidationError::InvalidGroup);
        }
    }
    let mut dup_ids = BTreeSet::new();
    let mut duplicate_members = BTreeSet::new();
    let mut duplicate_membership_count = 0_usize;
    for set in duplicate_sets {
        if !dup_ids.insert(set.id) {
            return Err(ValidationError::DuplicateIdentity);
        }
        if set.item_ids.len() > MAX_GROUP_MEMBERSHIPS
            || duplicate_membership_count.saturating_add(set.item_ids.len()) > MAX_GROUP_MEMBERSHIPS
        {
            return Err(ValidationError::InvalidDuplicateSet);
        }
        let mut members = BTreeSet::new();
        if set.item_ids.len() < 2
            || set.item_ids.iter().any(|id| {
                !ids.contains(id) || !members.insert(*id) || !duplicate_members.insert(*id)
            })
        {
            return Err(ValidationError::InvalidDuplicateSet);
        }
        let mut duplicate_size = None;
        for id in &set.item_ids {
            let item = item_by_id
                .get(id)
                .ok_or(ValidationError::InvalidDuplicateSet)?;
            let size = item
                .logical_bytes
                .ok_or(ValidationError::InvalidDuplicateSet)?;
            if item.kind != ItemKind::File
                || duplicate_size.is_some_and(|expected| expected != size)
            {
                return Err(ValidationError::InvalidDuplicateSet);
            }
            duplicate_size = Some(size);
        }
        duplicate_membership_count += set.item_ids.len();
        if duplicate_membership_count > MAX_GROUP_MEMBERSHIPS {
            return Err(ValidationError::InvalidDuplicateSet);
        }
        if set.survivor_id.is_some_and(|id| !members.contains(&id)) {
            return Err(ValidationError::InvalidDuplicateSet);
        }
    }
    if total_evidence > MAX_TOTAL_EVIDENCE {
        return Err(ValidationError::TooManyEvidence);
    }
    if total_blockers > MAX_TOTAL_BLOCKERS {
        return Err(ValidationError::InvalidBlocker);
    }
    let estimated_bytes = items
        .len()
        .saturating_mul(384)
        .saturating_add(
            groups
                .iter()
                .map(|g| g.item_ids.len().saturating_mul(64))
                .sum::<usize>(),
        )
        .saturating_add(
            duplicate_sets
                .iter()
                .map(|d| d.item_ids.len().saturating_mul(64))
                .sum::<usize>(),
        )
        .saturating_add(variable_text_bytes);
    if estimated_bytes > MAX_STATE_BYTES {
        return Err(ValidationError::SerializedStateTooLarge);
    }
    Ok(())
}

fn has_cycle_or_excess_depth(items: &[InventoryItem]) -> bool {
    let parents: BTreeMap<_, _> = items.iter().map(|i| (i.id, i.parent_id)).collect();
    for item in items {
        let mut seen = BTreeSet::new();
        let mut current = Some(item.id);
        let mut depth = 0;
        while let Some(id) = current {
            if !seen.insert(id) {
                return true;
            }
            depth += 1;
            if depth > MAX_TREE_DEPTH {
                return true;
            }
            current = parents.get(&id).copied().flatten();
        }
    }
    false
}

fn is_descendant_with_parents(
    parents: &BTreeMap<InventoryItemId, Option<InventoryItemId>>,
    child: InventoryItemId,
    ancestor: InventoryItemId,
) -> bool {
    let mut at = parents.get(&child).copied().flatten();
    while let Some(id) = at {
        if id == ancestor {
            return true;
        }
        at = parents.get(&id).copied().flatten();
    }
    false
}

fn project(
    plan_id: PlanId,
    revision_id: RevisionId,
    revision_number: u64,
    state: &State,
    can_undo: bool,
    can_redo: bool,
) -> EditorSnapshot {
    let by_id: BTreeMap<_, _> = state.items.iter().map(|i| (i.id, i)).collect();
    let mut order: Vec<_> = state.items.iter().collect();
    order.sort_by_key(|item| depth_of(item.id, &by_id));
    let excluded_roots: BTreeSet<_> = state
        .exclusions
        .values()
        .flat_map(|ids| ids.iter().copied())
        .collect();
    let missing_survivor = missing_survivor_members(state);
    let duplicate_survivors: BTreeSet<_> = state
        .duplicate_sets
        .iter()
        .filter_map(|set| set.survivor_id)
        .collect();
    let mut selected = BTreeSet::new();
    let mut effective = BTreeMap::new();
    let mut inherited_intent = BTreeMap::new();
    for item in order {
        let parent_intent = item
            .parent_id
            .and_then(|p| inherited_intent.get(&p).copied());
        let parent_included = item
            .parent_id
            .and_then(|p| effective.get(&p).copied())
            .unwrap_or(false);
        let explicit_choice = state.choices.get(&item.id).copied();
        let choice = explicit_choice.or(parent_intent).unwrap_or({
            if parent_included {
                Choice::Include
            } else {
                item.suggestion.choice
            }
        });
        let mut excluded = false;
        let mut ancestor = Some(item.id);
        while let Some(parent_id) = ancestor {
            if excluded_roots.contains(&parent_id) {
                excluded = true;
            }
            ancestor = by_id.get(&parent_id).and_then(|parent| parent.parent_id);
        }
        let duplicate_survivor = duplicate_survivors.contains(&item.id);
        let in_plan = state.membership.get(&item.id).copied().unwrap_or(false);
        let included = in_plan
            && !excluded
            && !duplicate_survivor
            && choice == Choice::Include
            && blockers_for_item(item, item.id, &missing_survivor).is_empty();
        let next_intent = if !in_plan || excluded || duplicate_survivor {
            Some(Choice::Keep)
        } else if included {
            Some(Choice::Include)
        } else if matches!(explicit_choice, Some(Choice::Keep | Choice::Investigate)) {
            explicit_choice
        } else if matches!(parent_intent, Some(Choice::Keep | Choice::Investigate)) {
            parent_intent
        } else {
            None
        };
        effective.insert(item.id, included);
        if let Some(intent) = next_intent {
            inherited_intent.insert(item.id, intent);
        }
        if included {
            selected.insert(item.id);
        }
    }
    let mut group_ids_by_item: BTreeMap<InventoryItemId, Vec<GroupId>> = BTreeMap::new();
    for group in &state.groups {
        for id in &group.item_ids {
            group_ids_by_item.entry(*id).or_default().push(group.id);
        }
    }
    let mut projections = Vec::with_capacity(state.items.len());
    for item in &state.items {
        projections.push(ItemProjection {
            item_id: item.id,
            parent_id: item.parent_id,
            label: item.label.clone(),
            kind: item.kind,
            logical_bytes: item.logical_bytes,
            suggestion: item.suggestion.clone(),
            user_choice: state.choices.get(&item.id).copied(),
            in_plan: state.membership.get(&item.id).copied().unwrap_or(false),
            effective_selected: effective.get(&item.id).copied().unwrap_or(false),
            blockers: blockers_for_item(item, item.id, &missing_survivor),
            evidence: item.evidence.clone(),
            excluded_descendant_ids: state
                .exclusions
                .get(&item.id)
                .map(|x| x.iter().copied().collect())
                .unwrap_or_default(),
            group_ids: group_ids_by_item.get(&item.id).cloned().unwrap_or_default(),
            recovery_lower_bytes: item.recovery_lower_bytes,
            recovery_upper_bytes: item.recovery_upper_bytes,
        });
    }
    let estimate = estimate(state, &selected);
    let children: BTreeSet<_> = state.items.iter().filter_map(|i| i.parent_id).collect();
    let mut group_selected_counts: BTreeMap<GroupId, usize> = BTreeMap::new();
    let mut group_known_bytes: BTreeMap<GroupId, u64> = BTreeMap::new();
    let mut group_shared_bytes: BTreeMap<GroupId, BTreeMap<String, u64>> = BTreeMap::new();
    for leaf in state
        .items
        .iter()
        .filter(|item| selected.contains(&item.id) && !children.contains(&item.id))
    {
        let mut relevant_groups = BTreeSet::new();
        let mut at = Some(leaf.id);
        while let Some(id) = at {
            if let Some(groups) = group_ids_by_item.get(&id) {
                relevant_groups.extend(groups.iter().copied());
            }
            at = by_id.get(&id).and_then(|item| item.parent_id);
        }
        for group_id in relevant_groups {
            *group_selected_counts.entry(group_id).or_default() += 1;
            if let Some(key) = &leaf.storage_identity {
                group_shared_bytes
                    .entry(group_id)
                    .or_default()
                    .entry(key.clone())
                    .and_modify(|bytes| *bytes = (*bytes).max(leaf.logical_bytes.unwrap_or(0)))
                    .or_insert(leaf.logical_bytes.unwrap_or(0));
            } else {
                let total = group_known_bytes.entry(group_id).or_default();
                *total = total.saturating_add(leaf.logical_bytes.unwrap_or(0));
            }
        }
    }
    let groups = state
        .groups
        .iter()
        .map(|g| {
            let members: Vec<_> = g
                .item_ids
                .iter()
                .filter_map(|id| by_id.get(id).copied())
                .collect();
            GroupProjection {
                id: g.id,
                label: g.label.clone(),
                member_ids: g.item_ids.clone(),
                selected_count: group_selected_counts.get(&g.id).copied().unwrap_or(0),
                selected_logical_bytes: group_known_bytes
                    .get(&g.id)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(
                        group_shared_bytes
                            .get(&g.id)
                            .map(|shared| shared.values().copied().fold(0_u64, u64::saturating_add))
                            .unwrap_or(0),
                    ),
                blocked_count: members
                    .iter()
                    .filter(|i| !blockers_for_item(i, i.id, &missing_survivor).is_empty())
                    .count(),
            }
        })
        .collect();
    let duplicates = state
        .duplicate_sets
        .iter()
        .map(|d| DuplicateProjection {
            id: d.id,
            member_ids: d.item_ids.clone(),
            survivor_id: d.survivor_id,
        })
        .collect();
    EditorSnapshot {
        schema_version: 1,
        kind: ProjectionKind::NonExecutableEditor,
        execution_available: false,
        plan_id,
        revision_id,
        revision_number,
        items: projections,
        groups,
        duplicates,
        totals: estimate,
        can_undo,
        can_redo,
    }
}

fn depth_of(id: InventoryItemId, by_id: &BTreeMap<InventoryItemId, &InventoryItem>) -> usize {
    let mut depth = 0;
    let mut current = by_id.get(&id).and_then(|item| item.parent_id);
    while let Some(parent) = current {
        depth += 1;
        current = by_id.get(&parent).and_then(|item| item.parent_id);
    }
    depth
}

fn estimate(state: &State, selected: &BTreeSet<InventoryItemId>) -> Estimate {
    let children: BTreeSet<_> = state.items.iter().filter_map(|i| i.parent_id).collect();
    let leaves: Vec<_> = state
        .items
        .iter()
        .filter(|i| selected.contains(&i.id) && !children.contains(&i.id))
        .collect();
    let mut in_plan_logical_bytes = 0_u64;
    let mut in_plan_shared_bytes: BTreeMap<String, u64> = BTreeMap::new();
    for item in state.items.iter().filter(|i| {
        state.membership.get(&i.id).copied().unwrap_or(false) && !children.contains(&i.id)
    }) {
        if let Some(key) = &item.storage_identity {
            in_plan_shared_bytes
                .entry(key.clone())
                .and_modify(|bytes| *bytes = (*bytes).max(item.logical_bytes.unwrap_or(0)))
                .or_insert(item.logical_bytes.unwrap_or(0));
        } else {
            in_plan_logical_bytes =
                in_plan_logical_bytes.saturating_add(item.logical_bytes.unwrap_or(0));
        }
    }
    in_plan_logical_bytes = in_plan_logical_bytes.saturating_add(
        in_plan_shared_bytes
            .values()
            .copied()
            .fold(0_u64, u64::saturating_add),
    );
    let mut logical = 0_u64;
    let mut upper_unknown = false;
    let mut recovery: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut independent = Vec::new();
    let mut logical_shared: BTreeMap<String, u64> = BTreeMap::new();
    let all_aliases: BTreeMap<String, Vec<InventoryItemId>> = {
        let mut map: BTreeMap<String, Vec<InventoryItemId>> = BTreeMap::new();
        for item in &state.items {
            if let Some(key) = &item.storage_identity {
                map.entry(key.clone()).or_default().push(item.id);
            }
        }
        map
    };
    for item in &leaves {
        if let Some(key) = &item.storage_identity {
            logical_shared
                .entry(key.clone())
                .and_modify(|v| *v = (*v).max(item.logical_bytes.unwrap_or(0)))
                .or_insert(item.logical_bytes.unwrap_or(0));
        } else {
            logical = logical.saturating_add(item.logical_bytes.unwrap_or(0));
        }
        match (item.recovery_lower_bytes, item.recovery_upper_bytes) {
            (Some(lo), Some(hi)) => {
                if let Some(key) = &item.storage_identity {
                    if all_aliases
                        .get(key)
                        .is_some_and(|aliases| aliases.iter().any(|id| !selected.contains(id)))
                    {
                        continue;
                    }
                    recovery
                        .entry(key.clone())
                        .and_modify(|v| {
                            v.0 = v.0.max(lo);
                            v.1 = v.1.max(hi);
                        })
                        .or_insert((lo, hi));
                } else {
                    independent.push((lo, hi));
                }
            }
            _ => upper_unknown = true,
        }
    }
    logical = logical.saturating_add(
        logical_shared
            .values()
            .copied()
            .fold(0_u64, u64::saturating_add),
    );
    let lower = independent
        .iter()
        .map(|v| v.0)
        .fold(0_u64, u64::saturating_add)
        .saturating_add(
            recovery
                .iter()
                .filter(|(key, _)| {
                    all_aliases
                        .get(*key)
                        .is_some_and(|aliases| aliases.len() == 1)
                })
                .map(|(_, v)| v.0)
                .fold(0_u64, u64::saturating_add),
        );
    let upper = if upper_unknown {
        None
    } else {
        Some(
            independent
                .iter()
                .map(|v| v.1)
                .chain(recovery.values().map(|v| v.1))
                .fold(0_u64, u64::saturating_add),
        )
    };
    Estimate {
        selected_count: leaves.len(),
        logical_selected_bytes: logical,
        in_plan_logical_bytes,
        recovery_lower_bytes: lower,
        recovery_upper_bytes: upper,
    }
}

fn check_state_size(state: &State) -> Result<(), ValidationError> {
    let variable_bytes = state
        .items
        .iter()
        .map(|item| {
            item.label
                .len()
                .saturating_add(item.suggestion.rationale.len())
                .saturating_add(item.storage_identity.as_ref().map_or(0, String::len))
                .saturating_add(
                    item.evidence
                        .iter()
                        .map(|e| e.summary.len().saturating_add(96))
                        .sum::<usize>(),
                )
                .saturating_add(
                    item.blockers
                        .iter()
                        .map(|b| {
                            b.code
                                .len()
                                .saturating_add(b.reason.len())
                                .saturating_add(64)
                        })
                        .sum::<usize>(),
                )
        })
        .sum::<usize>();
    let estimated = state
        .items
        .len()
        .saturating_mul(384)
        .saturating_add(variable_bytes)
        .saturating_add(
            state
                .groups
                .iter()
                .map(|g| {
                    g.label
                        .len()
                        .saturating_add(g.item_ids.len().saturating_mul(64))
                })
                .sum::<usize>(),
        )
        .saturating_add(
            state
                .duplicate_sets
                .iter()
                .map(|d| d.item_ids.len().saturating_mul(64))
                .sum::<usize>(),
        )
        .saturating_add(state.choices.len().saturating_mul(96))
        .saturating_add(state.membership.len().saturating_mul(96))
        .saturating_add(
            state
                .exclusions
                .values()
                .map(|ids| ids.len().saturating_mul(64))
                .sum::<usize>(),
        );
    if estimated > MAX_STATE_BYTES {
        return Err(ValidationError::SerializedStateTooLarge);
    }
    let json = serde_json::to_vec(state).map_err(|_| ValidationError::SerializedStateTooLarge)?;
    if json.len() > MAX_STATE_BYTES {
        Err(ValidationError::SerializedStateTooLarge)
    } else {
        Ok(())
    }
}

fn review_manifest_hash(manifest: &ReviewManifest<'_>) -> String {
    // Fixed v1 fields only; transient undo/redo flags and fresh review IDs are excluded.
    let bytes =
        serde_json::to_vec(manifest).expect("serializing fixed review manifest is infallible");
    format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        label: &str,
        parent_id: Option<InventoryItemId>,
        kind: ItemKind,
        choice: Choice,
        bytes: u64,
    ) -> InventoryItem {
        InventoryItem {
            id: InventoryItemId::new(),
            parent_id,
            label: label.to_owned(),
            kind,
            logical_bytes: Some(bytes),
            recovery_lower_bytes: Some(bytes / 2),
            recovery_upper_bytes: Some(bytes),
            storage_identity: None,
            blockers: Vec::new(),
            evidence: Vec::new(),
            suggestion: Suggestion {
                choice,
                rationale: "Synthetic rationale".to_owned(),
            },
        }
    }

    fn sample() -> PlanInput {
        let root = item("Folder", None, ItemKind::Folder, Choice::Include, 900);
        let mut keep = item("Keep me", Some(root.id), ItemKind::File, Choice::Keep, 300);
        let include = item(
            "Candidate",
            Some(root.id),
            ItemKind::File,
            Choice::Include,
            100,
        );
        let other = item(
            "Duplicate",
            Some(root.id),
            ItemKind::File,
            Choice::Include,
            100,
        );
        let mut blocked = item(
            "Blocked",
            Some(root.id),
            ItemKind::File,
            Choice::Include,
            400,
        );
        blocked.blockers.push(Blocker {
            code: "unsupported".into(),
            reason: "Unsupported operation".into(),
        });
        let group = PlanGroup {
            id: GroupId::new(),
            label: "Review group".into(),
            item_ids: vec![root.id, keep.id, include.id, other.id, blocked.id],
        };
        let duplicates = DuplicateSet {
            id: DuplicateGroupId::new(),
            item_ids: vec![include.id, other.id],
            survivor_id: Some(other.id),
        };
        keep.label = "Keep me".to_owned();
        PlanInput {
            plan_id: PlanId::new(),
            items: vec![root, keep, include, other, blocked],
            groups: vec![group],
            duplicate_sets: vec![duplicates],
        }
    }

    fn current(editor: &PlanEditor) -> RevisionId {
        editor.snapshot().revision_id
    }

    fn comparison_item(mut value: InventoryItem) -> InventoryItem {
        value.recovery_lower_bytes = None;
        value.recovery_upper_bytes = None;
        value.storage_identity = None;
        value.blockers.push(Blocker {
            code: "read_only_comparison".into(),
            reason: "Read-only duplicate comparison evidence".into(),
        });
        value.evidence.push(EvidenceRef {
            id: EvidenceId::new(),
            summary: "Native exact-content comparison; no cleanup authority".into(),
        });
        value.suggestion.choice = Choice::Keep;
        value
    }

    fn pytest_evidence_item(mut value: InventoryItem, evidence_id: EvidenceId) -> InventoryItem {
        value.recovery_lower_bytes = None;
        value.recovery_upper_bytes = None;
        value.storage_identity = None;
        value.blockers.push(Blocker {
            code: "read_only_pytest".into(),
            reason: "Read-only pytest cache format evidence; no cleanup authority".into(),
        });
        value.evidence.push(EvidenceRef {
            id: evidence_id,
            summary: "pytest-cache-format-v1; reference 9.2.0; format only".into(),
        });
        value.suggestion.choice = Choice::Keep;
        value
    }

    #[test]
    fn pytest_evidence_enriches_and_adds_items_without_replacing_user_state() {
        let mut existing = item("nodeids", None, ItemKind::File, Choice::Include, 64);
        existing.recovery_lower_bytes = None;
        existing.recovery_upper_bytes = None;
        let other = item("another", None, ItemKind::File, Choice::Include, 64);
        let duplicate_id = DuplicateGroupId::new();
        let set = DuplicateSet {
            id: duplicate_id,
            item_ids: vec![existing.id, other.id],
            survivor_id: None,
        };
        let mut editor = PlanEditor::new(PlanInput {
            plan_id: PlanId::new(),
            items: vec![existing.clone(), other],
            groups: vec![],
            duplicate_sets: vec![set],
        })
        .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: existing.id,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetMembership {
                    item_id: existing.id,
                    in_plan: false,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id: duplicate_id,
                    survivor_id: existing.id,
                },
            )
            .unwrap();
        let before = editor.snapshot();
        let new_item = pytest_evidence_item(
            item("lastfailed", None, ItemKind::File, Choice::Include, 48),
            EvidenceId::new(),
        );
        let evidence_id = EvidenceId::new();
        let incoming_existing = pytest_evidence_item(existing.clone(), evidence_id);
        let incoming = vec![incoming_existing.clone(), new_item.clone()];
        let enriched = editor
            .add_pytest_evidence(before.revision_id, incoming.clone())
            .unwrap();
        let old_row = enriched
            .items
            .iter()
            .find(|row| row.item_id == existing.id)
            .unwrap();
        assert_eq!(
            old_row.suggestion,
            before
                .items
                .iter()
                .find(|row| row.item_id == existing.id)
                .unwrap()
                .suggestion
        );
        assert!(!old_row.in_plan);
        assert_eq!(old_row.user_choice, Some(Choice::Keep));
        assert!(old_row
            .blockers
            .iter()
            .any(|blocker| blocker.code == "read_only_pytest"));
        assert_eq!(old_row.evidence.len(), existing.evidence.len() + 1);
        assert_eq!(
            enriched
                .duplicates
                .iter()
                .find(|row| row.id == duplicate_id)
                .unwrap()
                .survivor_id,
            Some(existing.id)
        );
        let new_row = enriched
            .items
            .iter()
            .find(|row| row.item_id == new_item.id)
            .unwrap();
        assert!(new_row.in_plan);
        assert_eq!(new_row.suggestion.choice, Choice::Keep);
        let idempotent = editor
            .add_pytest_evidence(enriched.revision_id, incoming)
            .unwrap();
        assert_eq!(idempotent.revision_id, enriched.revision_id);
        let mut conflicting = incoming_existing;
        conflicting.evidence.last_mut().unwrap().summary = "different evidence for same ID".into();
        assert_eq!(
            editor.add_pytest_evidence(idempotent.revision_id, vec![conflicting]),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(editor.snapshot(), idempotent);
    }

    #[test]
    fn pytest_evidence_rejects_stale_unsafe_duplicate_and_oversize_batches() {
        let mut editor = PlanEditor::new(PlanInput {
            plan_id: PlanId::new(),
            items: vec![],
            groups: vec![],
            duplicate_sets: vec![],
        })
        .unwrap();
        let before = editor.snapshot();
        let valid = pytest_evidence_item(
            item("cache", None, ItemKind::File, Choice::Include, 64),
            EvidenceId::new(),
        );
        assert_eq!(
            editor.add_pytest_evidence(RevisionId::new(), vec![valid.clone()]),
            Err(EditError::StaleRevision)
        );
        let mut unsafe_authority = valid.clone();
        unsafe_authority.storage_identity = Some("shared".into());
        assert_eq!(
            editor.add_pytest_evidence(before.revision_id, vec![unsafe_authority]),
            Err(EditError::InvalidInventory)
        );
        let mut too_large = valid.clone();
        too_large.logical_bytes = Some(65_537);
        assert_eq!(
            editor.add_pytest_evidence(before.revision_id, vec![too_large]),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(
            editor.add_pytest_evidence(
                before.revision_id,
                vec![valid.clone(), valid.clone(), valid]
            ),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(editor.snapshot(), before);
    }

    #[test]
    fn duplicate_evidence_enriches_additively_as_one_undoable_blocked_change() {
        let mut first = item("first.bin", None, ItemKind::File, Choice::Include, 10);
        first.recovery_lower_bytes = None;
        first.recovery_upper_bytes = None;
        let mut second = item("second.bin", None, ItemKind::File, Choice::Include, 10);
        second.recovery_lower_bytes = None;
        second.recovery_upper_bytes = None;
        let set_id = DuplicateGroupId::new();
        let set = DuplicateSet {
            id: set_id,
            item_ids: vec![first.id, second.id],
            survivor_id: None,
        };
        let input = PlanInput {
            plan_id: PlanId::new(),
            items: vec![first.clone(), second.clone()],
            groups: vec![],
            duplicate_sets: vec![set.clone()],
        };
        let mut editor = PlanEditor::new(input).unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: first.id,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetMembership {
                    item_id: second.id,
                    in_plan: false,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id: set_id,
                    survivor_id: first.id,
                },
            )
            .unwrap();
        let before = editor.snapshot();
        let third = item("third.bin", None, ItemKind::File, Choice::Include, 20);
        let fourth = item("fourth.bin", None, ItemKind::File, Choice::Include, 20);
        let new_set = DuplicateSet {
            id: DuplicateGroupId::new(),
            item_ids: vec![third.id, fourth.id],
            survivor_id: None,
        };
        let incoming = vec![
            comparison_item(first.clone()),
            comparison_item(second.clone()),
            comparison_item(third.clone()),
            comparison_item(fourth.clone()),
        ];
        let merged = editor
            .add_duplicate_evidence(
                before.revision_id,
                incoming.clone(),
                vec![set.clone(), new_set.clone()],
            )
            .unwrap();
        assert_eq!(merged.items.len(), 4);
        assert!(!merged.execution_available);
        for id in [first.id, second.id, third.id, fourth.id] {
            let row = merged.items.iter().find(|row| row.item_id == id).unwrap();
            assert!(row
                .blockers
                .iter()
                .any(|blocker| blocker.code == "read_only_comparison"));
            if let Some(previous) = before.items.iter().find(|previous| previous.item_id == id) {
                assert_eq!(row.suggestion, previous.suggestion);
            } else {
                assert_eq!(row.suggestion.choice, Choice::Keep);
            }
            assert!(!row.effective_selected);
        }
        assert_eq!(
            merged
                .items
                .iter()
                .find(|row| row.item_id == first.id)
                .unwrap()
                .user_choice,
            Some(Choice::Keep)
        );
        assert!(
            !merged
                .items
                .iter()
                .find(|row| row.item_id == second.id)
                .unwrap()
                .in_plan
        );
        assert_eq!(
            merged
                .duplicates
                .iter()
                .find(|row| row.id == set_id)
                .unwrap()
                .survivor_id,
            Some(first.id)
        );
        let duplicate_retry = editor
            .add_duplicate_evidence(merged.revision_id, incoming, vec![set, new_set])
            .unwrap();
        assert_eq!(duplicate_retry.revision_id, merged.revision_id);
        let overlapping = vec![
            comparison_item(first.clone()),
            comparison_item(second.clone()),
            comparison_item(third.clone()),
            comparison_item(fourth.clone()),
        ];
        let overlap_groups = vec![
            DuplicateSet {
                id: DuplicateGroupId::new(),
                item_ids: vec![first.id, third.id],
                survivor_id: None,
            },
            DuplicateSet {
                id: DuplicateGroupId::new(),
                item_ids: vec![second.id, fourth.id],
                survivor_id: None,
            },
        ];
        assert_eq!(
            editor.add_duplicate_evidence(duplicate_retry.revision_id, overlapping, overlap_groups),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(editor.snapshot(), duplicate_retry);
        let undone = editor.undo(merged.revision_id).unwrap();
        assert_eq!(undone.items.len(), before.items.len());
        assert_eq!(
            undone
                .duplicates
                .iter()
                .find(|row| row.id == set_id)
                .unwrap()
                .survivor_id,
            Some(first.id)
        );
        let redone = editor.redo(undone.revision_id).unwrap();
        assert_eq!(redone.items.len(), 4);
        assert_eq!(
            redone
                .duplicates
                .iter()
                .find(|row| row.id == set_id)
                .unwrap()
                .survivor_id,
            Some(first.id)
        );
    }

    #[test]
    fn duplicate_evidence_rejects_invalid_sets_and_stale_revision_without_mutation() {
        let mut editor = PlanEditor::new(PlanInput {
            plan_id: PlanId::new(),
            items: vec![],
            groups: vec![],
            duplicate_sets: vec![],
        })
        .unwrap();
        let before = editor.snapshot();
        let left = comparison_item(item("left", None, ItemKind::File, Choice::Keep, 5));
        let right = comparison_item(item("right", None, ItemKind::File, Choice::Keep, 5));
        let set = DuplicateSet {
            id: DuplicateGroupId::new(),
            item_ids: vec![left.id, right.id],
            survivor_id: None,
        };
        assert_eq!(
            editor.add_duplicate_evidence(
                RevisionId::new(),
                vec![left.clone(), right.clone()],
                vec![set.clone()]
            ),
            Err(EditError::StaleRevision)
        );
        assert_eq!(
            editor.add_duplicate_evidence(
                before.revision_id,
                vec![left.clone(), right.clone()],
                vec![]
            ),
            Err(EditError::InvalidInventory)
        );
        let mut unsafe_item = right.clone();
        unsafe_item.suggestion.choice = Choice::Include;
        assert_eq!(
            editor.add_duplicate_evidence(
                before.revision_id,
                vec![left.clone(), unsafe_item],
                vec![set.clone()]
            ),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(
            editor.add_duplicate_evidence(
                before.revision_id,
                vec![left.clone(), right.clone()],
                vec![DuplicateSet {
                    item_ids: vec![left.id, left.id],
                    ..set
                }]
            ),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(editor.snapshot(), before);
    }

    #[test]
    fn folder_group_counts_selected_leaves_consistently_with_whole_plan() {
        let folder = item("Folder", None, ItemKind::Folder, Choice::Include, 30);
        let first = item(
            "First",
            Some(folder.id),
            ItemKind::File,
            Choice::Include,
            10,
        );
        let second = item(
            "Second",
            Some(folder.id),
            ItemKind::File,
            Choice::Include,
            20,
        );
        let input = PlanInput {
            plan_id: PlanId::new(),
            groups: vec![PlanGroup {
                id: GroupId::new(),
                label: "Folder group".into(),
                item_ids: vec![folder.id],
            }],
            items: vec![folder, first, second],
            duplicate_sets: vec![],
        };
        let snapshot = PlanEditor::new(input).unwrap().snapshot();
        assert_eq!(snapshot.totals.selected_count, 2);
        assert_eq!(
            snapshot.groups[0].selected_count,
            snapshot.totals.selected_count
        );
        assert_eq!(
            snapshot.groups[0].selected_logical_bytes,
            snapshot.totals.logical_selected_bytes
        );
        assert_eq!(snapshot.totals.logical_selected_bytes, 30);
    }

    #[test]
    fn parent_selection_respects_keep_blocker_exclusion_survivor_and_union_accounting() {
        let input = sample();
        let root = input.items[0].id;
        let keep = input.items[1].id;
        let candidate = input.items[2].id;
        let survivor = input.items[3].id;
        let blocked = input.items[4].id;
        let set_id = input.duplicate_sets[0].id;
        let mut editor = PlanEditor::new(input).unwrap();
        let rev = current(&editor);
        editor
            .edit(
                rev,
                PlanEdit::SetChoice {
                    item_id: keep,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        let rev = current(&editor);
        editor
            .edit(
                rev,
                PlanEdit::ExcludeDescendants {
                    parent_id: root,
                    item_ids: vec![candidate],
                },
            )
            .unwrap();
        let snapshot = editor.snapshot();
        assert!(
            !snapshot
                .items
                .iter()
                .find(|x| x.item_id == candidate)
                .unwrap()
                .effective_selected
        );
        assert!(
            !snapshot
                .items
                .iter()
                .find(|x| x.item_id == keep)
                .unwrap()
                .effective_selected
        );
        assert!(
            !snapshot
                .items
                .iter()
                .find(|x| x.item_id == survivor)
                .unwrap()
                .effective_selected
        );
        assert!(
            !snapshot
                .items
                .iter()
                .find(|x| x.item_id == blocked)
                .unwrap()
                .effective_selected
        );
        assert_eq!(snapshot.totals.logical_selected_bytes, 0);
        assert_eq!(
            editor
                .state
                .duplicate_sets
                .iter()
                .find(|d| d.id == set_id)
                .unwrap()
                .survivor_id,
            Some(survivor)
        );
    }

    #[test]
    fn blocked_include_is_refused_and_group_edits_preserve_blockers() {
        let input = sample();
        let blocked = input.items[4].id;
        let group = input.groups[0].id;
        let mut editor = PlanEditor::new(input).unwrap();
        let outcome = editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: blocked,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert_eq!(outcome.blocked.len(), 1);
        assert!(!outcome.changed);
        let outcome = editor
            .edit(
                current(&editor),
                PlanEdit::SetGroupChoice {
                    group_id: group,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert_eq!(outcome.blocked.len(), 1);
        assert_eq!(outcome.blocked[0].item_id, blocked);
        assert!(
            !outcome
                .snapshot
                .items
                .iter()
                .find(|x| x.item_id == blocked)
                .unwrap()
                .effective_selected
        );
    }

    #[test]
    fn group_include_preserves_explicit_user_keep_until_individual_override() {
        let input = sample();
        let keep = input.items[1].id;
        let group = input.groups[0].id;
        let mut editor = PlanEditor::new(input).unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: keep,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        let outcome = editor
            .edit(
                current(&editor),
                PlanEdit::SetGroupChoice {
                    group_id: group,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert!(outcome
            .blocked
            .iter()
            .any(|row| row.item_id == keep && row.reason == "kept_by_user"));
        assert_eq!(
            outcome
                .snapshot
                .items
                .iter()
                .find(|row| row.item_id == keep)
                .unwrap()
                .user_choice,
            Some(Choice::Keep)
        );
        let outcome = editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: keep,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert!(
            outcome
                .snapshot
                .items
                .iter()
                .find(|row| row.item_id == keep)
                .unwrap()
                .effective_selected
        );
    }

    #[test]
    fn explicit_folder_keep_suppresses_child_suggestions_but_direct_child_include_can_override() {
        let folder = item(
            "Project folder",
            None,
            ItemKind::Folder,
            Choice::Include,
            30,
        );
        let a = item(
            "Generated",
            Some(folder.id),
            ItemKind::File,
            Choice::Include,
            10,
        );
        let b = item(
            "Notes",
            Some(folder.id),
            ItemKind::File,
            Choice::Include,
            20,
        );
        let child_id = a.id;
        let mut editor = PlanEditor::new(PlanInput {
            plan_id: PlanId::new(),
            items: vec![folder.clone(), a, b],
            groups: vec![],
            duplicate_sets: vec![],
        })
        .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: folder.id,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        assert!(editor
            .snapshot()
            .items
            .iter()
            .all(|row| !row.effective_selected));
        let snapshot = editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: child_id,
                    choice: Choice::Include,
                },
            )
            .unwrap()
            .snapshot;
        assert!(
            snapshot
                .items
                .iter()
                .find(|row| row.item_id == child_id)
                .unwrap()
                .effective_selected
        );
        assert!(
            !snapshot
                .items
                .iter()
                .find(|row| row.item_id == folder.id)
                .unwrap()
                .effective_selected
        );
    }

    #[test]
    fn stale_revision_noop_undo_redo_and_redo_invalidation() {
        let mut editor = PlanEditor::new(sample()).unwrap();
        let id = editor.state.items[1].id;
        let revision = current(&editor);
        let noop = editor
            .edit(
                revision,
                PlanEdit::SetMembership {
                    item_id: id,
                    in_plan: true,
                },
            )
            .unwrap();
        assert!(!noop.changed);
        assert_eq!(current(&editor), revision);
        let first = editor
            .edit(
                revision,
                PlanEdit::SetMembership {
                    item_id: id,
                    in_plan: false,
                },
            )
            .unwrap();
        let edited_revision = first.snapshot.revision_id;
        assert_ne!(edited_revision, revision);
        assert_eq!(
            editor
                .edit(
                    revision,
                    PlanEdit::SetMembership {
                        item_id: id,
                        in_plan: true
                    }
                )
                .unwrap_err(),
            EditError::StaleRevision
        );
        let undo = editor.undo(edited_revision).unwrap();
        assert_eq!(undo.revision_number, 3);
        assert_ne!(undo.revision_id, revision);
        let redone = editor.redo(undo.revision_id).unwrap();
        assert_eq!(redone.revision_number, 4);
        editor.undo(redone.revision_id).unwrap();
        let latest = current(&editor);
        editor
            .edit(
                latest,
                PlanEdit::SetChoice {
                    item_id: id,
                    choice: Choice::Investigate,
                },
            )
            .unwrap();
        assert_eq!(
            editor.redo(current(&editor)).unwrap_err(),
            EditError::EmptyHistory
        );
    }

    #[test]
    fn undo_redo_storage_is_capped_at_history_depth() {
        let mut editor = PlanEditor::new(sample()).unwrap();
        let id = editor.state.items[1].id;
        let mut in_plan = true;
        for _ in 0..(MAX_HISTORY + 5) {
            in_plan = !in_plan;
            editor
                .edit(
                    current(&editor),
                    PlanEdit::SetMembership {
                        item_id: id,
                        in_plan,
                    },
                )
                .unwrap();
        }
        let mut undo_count = 0;
        while editor.snapshot().can_undo {
            editor.undo(current(&editor)).unwrap();
            undo_count += 1;
        }
        assert_eq!(undo_count, MAX_HISTORY);
        assert_eq!(
            editor.undo(current(&editor)).unwrap_err(),
            EditError::EmptyHistory
        );
        assert!(editor.snapshot().can_redo);
    }

    #[test]
    fn refresh_preserves_user_choice_membership_exclusions_and_survivor_by_identity() {
        let mut input = sample();
        let folder = input.items[0].id;
        let candidate = input.items[2].id;
        let survivor = input.items[3].id;
        let duplicate_set_id = input.duplicate_sets[0].id;
        let mut editor = PlanEditor::new(input.clone()).unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: candidate,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id,
                    survivor_id: candidate,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetMembership {
                    item_id: survivor,
                    in_plan: false,
                },
            )
            .unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::ExcludeDescendants {
                    parent_id: folder,
                    item_ids: vec![candidate],
                },
            )
            .unwrap();
        input.items[2].suggestion = Suggestion {
            choice: Choice::Include,
            rationale: "Refreshed suggestion".into(),
        };
        let snapshot = editor
            .refresh(
                current(&editor),
                InventoryRefresh {
                    items: input.items,
                    groups: input.groups,
                    duplicate_sets: input.duplicate_sets,
                },
            )
            .unwrap();
        let row = snapshot
            .items
            .iter()
            .find(|i| i.item_id == candidate)
            .unwrap();
        assert_eq!(row.user_choice, Some(Choice::Keep));
        assert!(row.in_plan);
        assert!(row.excluded_descendant_ids.is_empty()); // exclusion is stored on the folder row
        assert_eq!(
            snapshot
                .items
                .iter()
                .find(|i| i.item_id == folder)
                .unwrap()
                .excluded_descendant_ids,
            vec![candidate]
        );
        assert!(
            !snapshot
                .items
                .iter()
                .find(|i| i.item_id == survivor)
                .unwrap()
                .in_plan
        );
        assert_eq!(snapshot.duplicates[0].survivor_id, Some(candidate));
    }

    #[test]
    fn trusted_add_items_preserves_user_state_and_participates_in_history() {
        let input = sample();
        let keep_id = input.items[1].id;
        let duplicate_set_id = input.duplicate_sets[0].id;
        let survivor_id = input.items[2].id;
        let mut editor = PlanEditor::new(input).unwrap();
        editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: keep_id,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        let selected = editor
            .edit(
                current(&editor),
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id,
                    survivor_id,
                },
            )
            .unwrap()
            .snapshot;
        let added = item(
            "Native metadata only",
            None,
            ItemKind::File,
            Choice::Keep,
            12,
        );
        let added_id = added.id;

        let after_add = editor.add_items(selected.revision_id, vec![added]).unwrap();
        assert_eq!(after_add.items.len(), selected.items.len() + 1);
        assert_eq!(after_add.duplicates[0].survivor_id, Some(survivor_id));
        assert_eq!(
            after_add
                .items
                .iter()
                .find(|row| row.item_id == keep_id)
                .unwrap()
                .user_choice,
            Some(Choice::Keep)
        );
        assert!(after_add.can_undo);

        let undone = editor.undo(after_add.revision_id).unwrap();
        assert_eq!(undone.items.len(), selected.items.len());
        assert_eq!(undone.duplicates[0].survivor_id, Some(survivor_id));
        let redone = editor.redo(undone.revision_id).unwrap();
        assert!(redone.items.iter().any(|row| row.item_id == added_id));
        assert_eq!(redone.duplicates[0].survivor_id, Some(survivor_id));
    }

    #[test]
    fn trusted_add_items_rejects_stale_duplicate_invalid_and_over_capacity_atomically() {
        let mut editor = PlanEditor::new(sample()).unwrap();
        let before = editor.snapshot();
        let existing_id = before.items[0].item_id;
        let duplicate = item("Duplicate ID", None, ItemKind::File, Choice::Keep, 1);
        let mut duplicate = duplicate;
        duplicate.id = existing_id;
        assert_eq!(
            editor.add_items(before.revision_id, vec![duplicate]),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(editor.snapshot(), before);

        let mut invalid = item("Invalid bounds", None, ItemKind::File, Choice::Keep, 1);
        invalid.recovery_lower_bytes = Some(2);
        invalid.recovery_upper_bytes = Some(1);
        assert_eq!(
            editor.add_items(before.revision_id, vec![invalid]),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(editor.snapshot(), before);

        assert_eq!(
            editor.add_items(
                RevisionId::new(),
                vec![item("Stale", None, ItemKind::File, Choice::Keep, 1)]
            ),
            Err(EditError::StaleRevision)
        );
        assert_eq!(editor.snapshot(), before);

        let mut full = PlanEditor::new(synthetic_fixture(MAX_ITEMS, 25).unwrap()).unwrap();
        let full_before = full.snapshot();
        assert_eq!(
            full.add_items(
                full_before.revision_id,
                vec![item("Over cap", None, ItemKind::File, Choice::Keep, 1)]
            ),
            Err(EditError::InvalidInventory)
        );
        assert_eq!(full.snapshot(), full_before);
    }

    #[test]
    fn malformed_cycles_depth_bounds_and_unknown_serde_fields_are_rejected() {
        let mut input = sample();
        let a = input.items[0].id;
        input.items[0].parent_id = Some(input.items[1].id);
        input.items[1].parent_id = Some(a);
        assert_eq!(
            PlanEditor::new(input).unwrap_err(),
            ValidationError::InvalidTree
        );

        let mut input = synthetic_fixture(66, 1).unwrap();
        for index in 0..65 {
            input.items[index].kind = ItemKind::Folder;
            input.items[index].parent_id = Some(input.items[index + 1].id);
        }
        input.items[65].parent_id = None;
        assert_eq!(
            PlanEditor::new(input).unwrap_err(),
            ValidationError::InvalidTree
        );

        let mut input = sample();
        input.items[0].recovery_lower_bytes = Some(4);
        input.items[0].recovery_upper_bytes = Some(2);
        assert_eq!(
            PlanEditor::new(input).unwrap_err(),
            ValidationError::InvalidBounds
        );

        let bad: Result<PlanInput, _> = serde_json::from_str(
            r#"{"planId":"bad","items":[],"groups":[],"duplicateSets":[],"extra":1}"#,
        );
        assert!(bad.is_err());
    }

    #[test]
    fn review_is_immutable_deterministic_and_non_executable() {
        let mut editor = PlanEditor::new(sample()).unwrap();
        let before = editor.freeze_review(current(&editor)).unwrap();
        assert_eq!(before.kind, ProjectionKind::NonExecutableEditor);
        assert!(!before.execution_available);
        let before_json = serde_json::to_vec(&before).unwrap();
        let id = editor.state.items[1].id;
        editor
            .edit(
                current(&editor),
                PlanEdit::SetMembership {
                    item_id: id,
                    in_plan: false,
                },
            )
            .unwrap();
        assert_eq!(serde_json::to_vec(&before).unwrap(), before_json);
        let same = editor.freeze_review(current(&editor)).unwrap();
        assert!(before.verify_content_hash());
        assert!(same.verify_content_hash());
        assert_ne!(same.content_hash, before.content_hash);
        let repeated = editor.freeze_review(current(&editor)).unwrap();
        assert_eq!(same.content_hash, repeated.content_hash);
        let mut tampered = same.clone();
        tampered.items[0].label.push('!');
        assert!(!tampered.verify_content_hash());
        let mut wrong_version = same.clone();
        wrong_version.review_version.push_str("-changed");
        assert!(!wrong_version.verify_content_hash());
        let mut executable = same;
        executable.execution_available = true;
        assert!(!executable.verify_content_hash());
    }

    #[test]
    fn duplicate_survivor_is_required_before_any_member_can_be_included() {
        let mut input = sample();
        input.duplicate_sets[0].survivor_id = None;
        let left = input.duplicate_sets[0].item_ids[0];
        let set_id = input.duplicate_sets[0].id;
        let mut editor = PlanEditor::new(input).unwrap();
        let row = editor
            .snapshot()
            .items
            .iter()
            .find(|item| item.item_id == left)
            .unwrap()
            .clone();
        assert!(!row.effective_selected);
        assert!(row
            .blockers
            .iter()
            .any(|b| b.code == "duplicate_survivor_required"));
        let attempted = editor
            .edit(
                current(&editor),
                PlanEdit::SetChoice {
                    item_id: left,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert_eq!(
            attempted.blocked[0].reason,
            "Choose which duplicate to keep before including this item"
        );
        editor
            .edit(
                current(&editor),
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id: set_id,
                    survivor_id: input_dup_other(&editor.state, set_id, left),
                },
            )
            .unwrap();
        assert!(
            editor
                .snapshot()
                .items
                .iter()
                .find(|item| item.item_id == left)
                .unwrap()
                .effective_selected
        );
    }

    fn input_dup_other(
        state: &State,
        set_id: DuplicateGroupId,
        excluded: InventoryItemId,
    ) -> InventoryItemId {
        *state
            .duplicate_sets
            .iter()
            .find(|set| set.id == set_id)
            .unwrap()
            .item_ids
            .iter()
            .find(|id| **id != excluded)
            .unwrap()
    }

    #[test]
    fn external_dto_uses_frozen_camel_case_and_lowercase_choice_wire_names() {
        let editor = PlanEditor::new(sample()).unwrap();
        let snapshot = serde_json::to_value(editor.snapshot()).unwrap();
        assert_eq!(snapshot["schemaVersion"], 1);
        assert_eq!(snapshot["kind"], "non_executable_editor");
        assert_eq!(snapshot["executionAvailable"], false);
        assert!(snapshot.get("revisionId").is_some());
        assert!(snapshot.get("revision_id").is_none());
        let edit = serde_json::to_value(PlanEdit::SetChoice {
            item_id: editor.state.items[0].id,
            choice: Choice::Keep,
        })
        .unwrap();
        assert_eq!(edit["kind"], "setChoice");
        assert_eq!(edit["choice"], "keep");
        assert!(edit.get("itemId").is_some());
    }

    #[test]
    fn shared_storage_and_parent_child_are_not_double_counted() {
        let root = item("root", None, ItemKind::Folder, Choice::Include, 1_000);
        let mut left = item("left", Some(root.id), ItemKind::File, Choice::Include, 400);
        let mut right = item("right", Some(root.id), ItemKind::File, Choice::Include, 400);
        left.storage_identity = Some("same-object".into());
        right.storage_identity = Some("same-object".into());
        let input = PlanInput {
            plan_id: PlanId::new(),
            items: vec![root, left.clone(), right.clone()],
            groups: vec![],
            duplicate_sets: vec![],
        };
        let mut editor = PlanEditor::new(input).unwrap();
        let snapshot = editor.snapshot();
        assert_eq!(snapshot.totals.logical_selected_bytes, 400);
        assert_eq!(snapshot.totals.recovery_lower_bytes, 0);
        assert_eq!(snapshot.totals.recovery_upper_bytes, Some(400));
        editor
            .edit(
                current(&editor),
                PlanEdit::SetMembership {
                    item_id: right.id,
                    in_plan: false,
                },
            )
            .unwrap();
        assert_eq!(editor.snapshot().totals.recovery_upper_bytes, Some(0));
    }

    #[test]
    fn archive_roundtrip_preserves_revision_history_and_rejects_tampering() {
        let mut editor = PlanEditor::new(sample()).unwrap();
        let id = editor.state.items[1].id;
        editor
            .edit(
                current(&editor),
                PlanEdit::SetMembership {
                    item_id: id,
                    in_plan: false,
                },
            )
            .unwrap();
        let bytes = editor.export_document_json().unwrap();
        let mut restored = PlanEditor::import_document_json(&bytes).unwrap();
        assert_eq!(
            restored.snapshot().revision_id,
            editor.snapshot().revision_id
        );
        assert_eq!(restored.snapshot(), editor.snapshot());
        let undo = restored.undo(current(&restored)).unwrap();
        assert_eq!(undo.revision_number, 3);
        assert_eq!(
            restored.redo(current(&restored)).unwrap().revision_number,
            4
        );
        assert!(PlanEditor::import_document_json(&bytes[..bytes.len() - 1]).is_err());
        assert!(PlanEditor::import_document_json(&vec![b' '; MAX_SERIALIZED_BYTES + 1]).is_err());
    }

    #[test]
    fn representative_and_ten_thousand_item_fixtures_are_bounded() {
        let fixture = synthetic_fixture(1_000, 25).unwrap();
        let editor = PlanEditor::new(fixture).unwrap();
        assert_eq!(editor.snapshot().items.len(), 1_000);
        assert_eq!(editor.snapshot().groups.len(), 25);
        let stress = PlanEditor::new(synthetic_fixture(10_000, 25).unwrap()).unwrap();
        assert_eq!(stress.snapshot().items.len(), 10_000);
        assert_eq!(stress.snapshot().kind, ProjectionKind::NonExecutableEditor);
    }

    #[test]
    fn dense_ten_thousand_member_group_with_five_thousand_duplicate_pairs_scales() {
        let items: Vec<_> = (0..MAX_ITEMS)
            .map(|index| {
                item(
                    &format!("Synthetic dense item {index}"),
                    None,
                    ItemKind::File,
                    Choice::Include,
                    1_024,
                )
            })
            .collect();
        let item_ids: Vec<_> = items.iter().map(|entry| entry.id).collect();
        let group = PlanGroup {
            id: GroupId::new(),
            label: "All synthetic items".to_owned(),
            item_ids: item_ids.clone(),
        };
        let duplicate_sets = item_ids
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| DuplicateSet {
                id: DuplicateGroupId::new(),
                item_ids: pair.to_vec(),
                survivor_id: Some(pair[0]),
            })
            .collect();
        let input = PlanInput {
            plan_id: PlanId::new(),
            items,
            groups: vec![group.clone()],
            duplicate_sets,
        };

        let started = std::time::Instant::now();
        let mut editor = PlanEditor::new(input).unwrap();
        let outcome = editor
            .edit(
                current(&editor),
                PlanEdit::SetGroupChoice {
                    group_id: group.id,
                    choice: Choice::Include,
                },
            )
            .unwrap();
        assert_eq!(outcome.snapshot.items.len(), MAX_ITEMS);
        assert_eq!(outcome.snapshot.duplicates.len(), MAX_ITEMS / 2);
        assert_eq!(outcome.snapshot.totals.selected_count, MAX_ITEMS / 2);
        assert!(!outcome.snapshot.execution_available);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(12),
            "dense projection exceeded the 12 second debug budget: {:?}",
            started.elapsed()
        );
    }
}
