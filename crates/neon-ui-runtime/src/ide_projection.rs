//! Phase 5: Neon IDE projection trees.
//!
//! Two domain-state projections (`FileTreeProjection`,
//! `AgentWorkbenchProjection`) compose into one `IdeWorkspaceProjection`
//! document. Domain mutations never trigger a full Flow submit: the
//! workspace diffs its last-emitted tree with the Phase 2 keyed diff and
//! emits a `UiPatch` that rides the existing `ui.flow.patch` pipeline.
//!
//! Invariants that keep patches minimal and IR-faithful:
//!
//! * Node keys are path-safe (`letters, digits, '.', '_', '-'`) and derived
//!   reversibly from domain identities, so keeps, removes and inserts follow
//!   stable keys. `MoveNode` is avoided by design: the IR `move` contract
//!   appends to the destination and cannot honor an index, so the
//!   projections order lists through inserted/removed rows only.
//! * All dynamic rows are `text` or `panel` nodes whose only varying fields
//!   are inside the IR `Set` whitelist (`value`, `fill`, `opacity`,
//!   `visible`, `enabled`, `w`, `h`). Layout coordinates (`x`/`y`) never
//!   encode domain state, so identity changes stay rare.
//! * `initial_source()` emits exactly the tree `project()` builds; a
//!   round-trip test pins parser parity, which also means every inserted
//!   patch payload is shape-identical to parsed siblings.
//! * A root identity change (the one case where keyed diff refuses) surfaces
//!   as `IdeProjectionUpdate::FullSubmitRequired` with a fresh source, so
//!   callers have a single, explicit remount path.

use std::collections::BTreeSet;

use neon_ui_schema::{
    TextRef, UiBounds, UiEasing, UiIrDocument, UiLayout, UiLayoutMode, UiNode, UiNodeId,
    UiNodeKind, UiPatch, UiPatchOp, UiStyle, UiTransition, UiTransitionState,
};

use crate::nui_flow::{apply_ui_patch, parse_nui_flow};
use crate::ui_keyed_diff::{
    UiDiffRootMismatch, build_ui_patch, diff_projection_trees, summarize_operations,
};

pub const FILE_TREE_ROW_HEIGHT: f32 = 20.0;
pub const TRANSPARENT_FILL: &str = "#00000000";
pub const SELECTED_FILL: &str = "#2f5d8a";
pub const BUSY_OPACITY: f32 = 0.45;
/// Terminal-status row tints. The fill change is also what physically starts
/// the renderer's one-shot motion track (the engine only animates when the
/// target visual moved), so the sweep/flash is bound to the state change.
pub const COMPLETED_FILL: &str = "#1d3a24";
pub const FAILED_FILL: &str = "#3a1d1d";

/// Encodes an arbitrary domain identity into a valid Flow node key. The
/// mapping is injective: `_` doubles, `.` and `-` get short tags, and every
/// other non-alphanumeric byte becomes `_x<hex>`.
pub fn encode_key_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for character in segment.chars() {
        match character {
            '_' => out.push_str("__"),
            '.' => out.push_str("_d"),
            '-' => out.push_str("_h"),
            other if other.is_ascii_alphanumeric() => out.push(other),
            other => out.push_str(&format!("_x{:x}", other as u32)),
        }
    }
    out
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_key_segment)
        .collect::<Vec<_>>()
        .join(".")
}

fn panel(key: &str, mode: UiLayoutMode, width: f32, height: f32, fill: &str) -> UiNode {
    UiNode {
        node_id: UiNodeId(key.into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
        layout: Some(UiLayout {
            mode,
            ..UiLayout::default()
        }),
        visible: true,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: style_with(fill, 1.0),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: Default::default(),
        children: Vec::new(),
    }
}

fn style_with(fill: &str, opacity: f32) -> UiStyle {
    UiStyle {
        background_color: parse_hex_rgba(fill),
        opacity,
        ..UiStyle::default()
    }
}

fn row(
    key: &str,
    value: String,
    width: f32,
    height: f32,
    fill: &str,
    opacity: f32,
    visible: bool,
) -> UiNode {
    UiNode {
        node_id: UiNodeId(key.into()),
        kind: UiNodeKind::Label,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            width,
            height,
        },
        layout: Some(UiLayout::default()),
        visible,
        enabled: true,
        text_key: None,
        text: Some(TextRef::Literal { value }),
        image: None,
        surface: None,
        style: style_with(fill, opacity),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: Default::default(),
        children: Vec::new(),
    }
}

/// Parses `#rrggbb` or `#rrggbbaa` into the same normalized floats the Flow
/// parser produces (`channel / 255.0`), keeping builder and parsed node
/// shapes identical.
fn parse_hex_rgba(text: &str) -> [f32; 4] {
    let digits = text.trim_start_matches('#');
    assert!(
        digits.len() == 6 || digits.len() == 8,
        "projection colors must be #RRGGBB or #RRGGBBAA"
    );
    let byte = |start: usize| digits[start..start + 2].parse::<u32>().unwrap_or(0) as f32 / 255.0;
    [
        byte(0),
        byte(2),
        byte(4),
        if digits.len() == 8 { byte(6) } else { 1.0 },
    ]
}

fn format_hex(color: [f32; 4]) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    let (red, green, blue, alpha) = (
        channel(color[0]),
        channel(color[1]),
        channel(color[2]),
        channel(color[3]),
    );
    if alpha == 255 {
        format!("#{red:02x}{green:02x}{blue:02x}")
    } else {
        format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}")
    }
}

fn format_number(value: f32) -> String {
    if value == value.round() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn escape_flow_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

/// File-tree domain state. Entries are stored in display order (parents
/// before children); visibility follows the expanded-directory set.
#[derive(Clone, Debug)]
pub struct FileTreeProjection {
    entries: Vec<FileTreeEntry>,
    expanded: BTreeSet<String>,
    selected: Option<String>,
    busy: BTreeSet<String>,
    width: f32,
    height: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTreeEntry {
    pub path: String,
    pub is_dir: bool,
}

impl FileTreeEntry {
    pub fn file(path: &str) -> Self {
        Self {
            path: path.into(),
            is_dir: false,
        }
    }
    pub fn dir(path: &str) -> Self {
        Self {
            path: path.into(),
            is_dir: true,
        }
    }
    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
    fn depth(&self) -> usize {
        self.path.matches('/').count()
    }
}

impl Default for FileTreeProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl FileTreeProjection {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            expanded: BTreeSet::new(),
            selected: None,
            busy: BTreeSet::new(),
            width: 260.0,
            height: 900.0,
        }
    }

    /// Replaces the flat entry list (the `scan_files` result). Order is the
    /// caller's display order; the projection preserves it verbatim so
    /// localized scans produce localized keyed diffs.
    pub fn set_entries(&mut self, entries: Vec<FileTreeEntry>) {
        self.entries = entries;
    }

    pub fn select(&mut self, path: Option<String>) {
        self.selected = path;
    }

    pub fn set_expanded(&mut self, dir: &str, expanded: bool) {
        if expanded {
            self.expanded.insert(dir.to_owned());
        } else {
            self.expanded.remove(dir);
        }
    }

    /// Visual operation state (importing, indexing): dims the row without
    /// touching its identity or text content.
    pub fn set_busy(&mut self, path: &str, busy: bool) {
        if busy {
            self.busy.insert(path.to_owned());
        } else {
            self.busy.remove(path);
        }
    }

    fn is_visible(&self, entry: &FileTreeEntry) -> bool {
        let mut cursor = entry.path.as_str();
        while let Some((parent, _)) = cursor.rsplit_once('/') {
            if !self.expanded.contains(parent) {
                return false;
            }
            cursor = parent;
        }
        true
    }

    fn row_node(&self, entry: &FileTreeEntry) -> UiNode {
        let marker = if entry.is_dir {
            if self.expanded.contains(&entry.path) {
                "-"
            } else {
                "+"
            }
        } else {
            ""
        };
        let value = if entry.is_dir {
            format!("{}{marker} {}", "  ".repeat(entry.depth()), entry.name())
        } else {
            format!("{}{}", "  ".repeat(entry.depth()), entry.name())
        };
        row(
            &format!("row.{}", encode_path(&entry.path)),
            value,
            self.width,
            FILE_TREE_ROW_HEIGHT,
            if self.selected.as_deref() == Some(entry.path.as_str()) {
                SELECTED_FILL
            } else {
                TRANSPARENT_FILL
            },
            if self.busy.contains(&entry.path) {
                BUSY_OPACITY
            } else {
                1.0
            },
            true,
        )
    }

    /// Builds the current sidebar subtree: one panel holding the visible
    /// rows in display order.
    pub fn project(&self) -> UiNode {
        let mut sidebar = panel(
            "sidebar",
            UiLayoutMode::Column,
            self.width,
            self.height,
            "#14181f",
        );
        sidebar.children = self
            .entries
            .iter()
            .filter(|entry| self.is_visible(entry))
            .map(|entry| self.row_node(entry))
            .collect();
        sidebar
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::Completed => "done",
            TaskStatus::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentTask {
    pub plan: String,
    pub name: String,
    pub status: TaskStatus,
    /// Optional same-plan dependency: the task row only appears in the plan
    /// list once its dependency reports `Completed`.
    pub depends_on: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSection {
    Tasks,
    Records,
}

/// Agent workbench domain state with the plan's stable node keys:
/// `agent.header`, `agent.status`, `agent.plan.list`, `task.<plan>.<task>`,
/// `transaction.<id>`, `change.<id>`, `approval.<id>`.
#[derive(Clone, Debug)]
pub struct AgentWorkbenchProjection {
    header: String,
    status_line: String,
    tasks: Vec<AgentTask>,
    transactions: Vec<(String, String)>,
    changes: Vec<(String, String)>,
    approvals: Vec<(String, String)>,
    hidden_sections: BTreeSet<&'static str>,
    /// One-shot motion intents bound to observed status changes. Drained by
    /// `IdeWorkspaceProjection::sync` and emitted as `StartTransition` ops;
    /// never part of the compared tree.
    pending_transitions: Vec<(String, UiTransition)>,
    motion_seq: u64,
    width: f32,
    height: f32,
}

impl Default for AgentWorkbenchProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentWorkbenchProjection {
    pub fn new() -> Self {
        Self {
            header: "Agents".into(),
            status_line: "idle".into(),
            tasks: Vec::new(),
            transactions: Vec::new(),
            changes: Vec::new(),
            approvals: Vec::new(),
            hidden_sections: BTreeSet::new(),
            pending_transitions: Vec::new(),
            motion_seq: 0,
            width: 940.0,
            height: 900.0,
        }
    }

    pub fn set_status_line(&mut self, status: impl Into<String>) {
        self.status_line = status.into();
    }

    pub fn add_task(&mut self, task: AgentTask) {
        self.tasks
            .retain(|existing| !(existing.plan == task.plan && existing.name == task.name));
        self.tasks.push(task);
    }

    pub fn remove_task(&mut self, plan: &str, name: &str) {
        self.tasks
            .retain(|task| !(task.plan == plan && task.name == name));
    }

    /// Status transitions are property-only: the row keeps its key, only the
    /// literal text changes. Newly-satisfied dependents arrive as inserts.
    /// Terminal status changes additionally enqueue a one-shot entry motion
    /// (success sweep / error flash) bound to this observation, never a
    /// persistent animation.
    pub fn set_task_status(&mut self, plan: &str, name: &str, status: TaskStatus) {
        let Some(index) = self
            .tasks
            .iter()
            .position(|task| task.plan == plan && task.name == name)
        else {
            return;
        };
        if self.tasks[index].status == status {
            return;
        }
        self.tasks[index].status = status;
        if !self.dependency_satisfied(&self.tasks[index]) {
            // The row is not in the tree yet; its text arrives with the
            // insert and there is nothing on screen to animate.
            return;
        }
        let motion = match status {
            TaskStatus::Completed => Some(("success-sweep", 0.6, 240_u32)),
            TaskStatus::Failed => Some(("error-flash", 0.25, 180)),
            TaskStatus::Queued | TaskStatus::Running => None,
        };
        let Some((prefix, from_opacity, duration_ms)) = motion else {
            return;
        };
        self.motion_seq += 1;
        let key = format!(
            "task.{}.{}",
            encode_key_segment(plan),
            encode_key_segment(name)
        );
        self.pending_transitions.push((
            key,
            UiTransition {
                delay_ms: 0,
                duration_ms,
                easing: UiEasing::EaseOut,
                from: UiTransitionState {
                    opacity: Some(from_opacity),
                    ..Default::default()
                },
                motion_key: Some(format!("{prefix}-{}", self.motion_seq)),
                timeline: None,
            },
        ));
    }

    /// Retrying a failed task removes the retry row and returns the row to
    /// `running` text; a retry press is a state change, not an animation.
    pub fn retry_task(&mut self, plan: &str, name: &str) {
        self.set_task_status(plan, name, TaskStatus::Running);
    }

    pub(crate) fn take_pending_transitions(&mut self) -> Vec<(String, UiTransition)> {
        std::mem::take(&mut self.pending_transitions)
    }

    /// Completing a task also auto-starts its queued dependents, so one
    /// domain event yields exactly one property set plus one insert per
    /// released task.
    pub fn complete_task(&mut self, plan: &str, name: &str) {
        self.set_task_status(plan, name, TaskStatus::Completed);
        let released: Vec<String> = self
            .tasks
            .iter()
            .filter(|task| {
                task.plan == plan
                    && task.depends_on.as_deref() == Some(name)
                    && task.status == TaskStatus::Queued
            })
            .map(|task| task.name.clone())
            .collect();
        for task_name in released {
            if let Some(task) = self
                .tasks
                .iter_mut()
                .find(|task| task.plan == plan && task.name == task_name)
            {
                task.status = TaskStatus::Running;
            }
        }
    }

    pub fn record_transaction(&mut self, id: impl Into<String>, label: impl Into<String>) {
        push_entry(&mut self.transactions, id.into(), label.into());
    }

    pub fn record_change(&mut self, id: impl Into<String>, label: impl Into<String>) {
        push_entry(&mut self.changes, id.into(), label.into());
    }

    pub fn record_approval(&mut self, id: impl Into<String>, label: impl Into<String>) {
        push_entry(&mut self.approvals, id.into(), label.into());
    }

    pub fn resolve_approval(&mut self, id: &str, accepted: bool) {
        if let Some((_, label)) = self
            .approvals
            .iter_mut()
            .find(|(entry_id, _)| entry_id == id)
        {
            *label = format!(
                "{label} {}",
                if accepted { "[approved]" } else { "[denied]" }
            );
        }
    }

    /// Panel switching maps to section visibility: hiding a section is one
    /// `visible` set on its container, never a workspace rebuild.
    pub fn set_section_visible(&mut self, section: AgentSection, visible: bool) {
        let key = match section {
            AgentSection::Tasks => "tasks",
            AgentSection::Records => "records",
        };
        if visible {
            self.hidden_sections.remove(key);
        } else {
            self.hidden_sections.insert(key);
        }
    }

    fn dependency_satisfied(&self, task: &AgentTask) -> bool {
        let Some(dependency) = &task.depends_on else {
            return true;
        };
        self.tasks.iter().any(|other| {
            other.plan == task.plan
                && &other.name == dependency
                && other.status == TaskStatus::Completed
        })
    }

    fn task_row(task: &AgentTask, width: f32) -> UiNode {
        let fill = match task.status {
            TaskStatus::Completed => COMPLETED_FILL,
            TaskStatus::Failed => FAILED_FILL,
            TaskStatus::Queued | TaskStatus::Running => TRANSPARENT_FILL,
        };
        row(
            &format!(
                "task.{}.{}",
                encode_key_segment(&task.plan),
                encode_key_segment(&task.name)
            ),
            format!("{} / {}: {}", task.plan, task.name, task.status.label()),
            width,
            FILE_TREE_ROW_HEIGHT,
            fill,
            1.0,
            true,
        )
    }

    /// A failed task carries an explicit action row, never color alone: the
    /// label states what failed and the node is a real Button the hit
    /// tester can resolve.
    fn retry_row(task: &AgentTask, width: f32) -> UiNode {
        let mut node = row(
            &format!(
                "task.{}.{}.retry",
                encode_key_segment(&task.plan),
                encode_key_segment(&task.name)
            ),
            format!("retry {} / {}", task.plan, task.name),
            width,
            FILE_TREE_ROW_HEIGHT,
            FAILED_FILL,
            1.0,
            true,
        );
        node.kind = UiNodeKind::Button;
        node
    }

    fn list_panel(key: &str, width: f32, height: f32, rows: Vec<UiNode>) -> UiNode {
        let mut container = panel(key, UiLayoutMode::Column, width, height, TRANSPARENT_FILL);
        container.children = rows;
        container
    }

    fn section_panel(key: &str, width: f32, visible: bool, children: Vec<UiNode>) -> UiNode {
        let mut section = panel(key, UiLayoutMode::Column, width, 300.0, TRANSPARENT_FILL);
        section.visible = visible;
        section.children = children;
        section
    }

    pub fn project(&self) -> UiNode {
        let mut agent = panel(
            "agent",
            UiLayoutMode::Column,
            self.width,
            self.height,
            "#10141a",
        );
        let header = row(
            "agent.header",
            self.header.clone(),
            self.width,
            28.0,
            TRANSPARENT_FILL,
            1.0,
            true,
        );
        let status = row(
            "agent.status",
            self.status_line.clone(),
            self.width,
            FILE_TREE_ROW_HEIGHT,
            TRANSPARENT_FILL,
            1.0,
            true,
        );
        let task_rows: Vec<UiNode> = self
            .tasks
            .iter()
            .filter(|task| self.dependency_satisfied(task))
            .flat_map(|task| {
                let mut rows = vec![Self::task_row(task, self.width)];
                if task.status == TaskStatus::Failed {
                    rows.push(Self::retry_row(task, self.width));
                }
                rows
            })
            .collect();
        let plan_list = Self::list_panel("agent.plan.list", self.width, 300.0, task_rows);
        let transactions = self
            .transactions
            .iter()
            .map(|(id, label)| {
                row(
                    &format!("transaction.{}", encode_key_segment(id)),
                    format!("transaction {id}: {label}"),
                    self.width,
                    FILE_TREE_ROW_HEIGHT,
                    TRANSPARENT_FILL,
                    1.0,
                    true,
                )
            })
            .collect();
        let changes = self
            .changes
            .iter()
            .map(|(id, label)| {
                row(
                    &format!("change.{}", encode_key_segment(id)),
                    format!("change {id}: {label}"),
                    self.width,
                    FILE_TREE_ROW_HEIGHT,
                    TRANSPARENT_FILL,
                    1.0,
                    true,
                )
            })
            .collect();
        let approvals = self
            .approvals
            .iter()
            .map(|(id, label)| {
                row(
                    &format!("approval.{}", encode_key_segment(id)),
                    format!("approval {id}: {label}"),
                    self.width,
                    FILE_TREE_ROW_HEIGHT,
                    TRANSPARENT_FILL,
                    1.0,
                    true,
                )
            })
            .collect();
        let records = Self::section_panel(
            "agent.section.records",
            self.width,
            !self.hidden_sections.contains("records"),
            vec![
                Self::list_panel("agent.transactions", self.width, 100.0, transactions),
                Self::list_panel("agent.changes", self.width, 100.0, changes),
                Self::list_panel("agent.approvals", self.width, 100.0, approvals),
            ],
        );
        let tasks_section = Self::section_panel(
            "agent.section.tasks",
            self.width,
            !self.hidden_sections.contains("tasks"),
            vec![plan_list],
        );
        agent.children = vec![header, status, tasks_section, records];
        agent
    }
}

fn push_entry(entries: &mut Vec<(String, String)>, id: String, label: String) {
    entries.retain(|(existing, _)| *existing != id);
    entries.push((id, label));
}

/// Result of reconciling the workspace after domain mutations.
#[derive(Clone, Debug, PartialEq)]
pub enum IdeProjectionUpdate {
    /// Nothing observable changed; do not touch the pipeline at all.
    NoChange,
    /// A keyed `UiPatch` to send through `ui.flow.patch`.
    Patch(UiPatch),
    /// The root identity changed (or nothing was adopted yet): the only
    /// correct move is one full `ui.flow.submit` with this source.
    FullSubmitRequired { source: String, reason: String },
}

/// Owns the composed IDE document: `workspace` root with the file-tree
/// sidebar and the agent workbench as the only children. All diffing happens
/// against this projection's own last-emitted tree, so neither panel can
/// force the other to remount.
#[derive(Clone, Debug)]
pub struct IdeWorkspaceProjection {
    surface_id: String,
    revision: u64,
    width: f32,
    height: f32,
    pub files: FileTreeProjection,
    pub agent: AgentWorkbenchProjection,
    baseline: Option<UiIrDocument>,
}

impl IdeWorkspaceProjection {
    pub fn new(surface_id: impl Into<String>, revision: u64) -> Self {
        Self {
            surface_id: surface_id.into(),
            revision,
            width: 1200.0,
            height: 900.0,
            files: FileTreeProjection::new(),
            agent: AgentWorkbenchProjection::new(),
            baseline: None,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn current_root(&self) -> UiNode {
        let mut workspace = panel(
            "workspace",
            UiLayoutMode::Row,
            self.width,
            self.height,
            "#0f1115",
        );
        workspace.children = vec![self.files.project(), self.agent.project()];
        workspace
    }

    /// Emits the canonical Flow source for the current tree. The submitted
    /// document and the projection baseline are the same content.
    pub fn initial_source(&self) -> String {
        let mut out = String::new();
        out.push_str("version 1\n");
        out.push_str(&format!(
            "surface {} revision {revision}\n",
            self.surface_id,
            revision = self.revision
        ));
        out.push_str("flow ide\n");
        out.push_str(
            "budget nodes=4096 bindings=4096 instances=4096 text=4096 glyphs=65536 events=64 \
             clips=4096\n",
        );
        write_flow_node(&self.current_root(), 0, true, &mut out);
        out
    }

    /// Adopts the submitted document as the diff baseline. Call after the
    /// first `ui.flow.submit` with `initial_source()` succeeded.
    pub fn adopt_baseline(&mut self) -> Result<(), String> {
        let parsed =
            parse_nui_flow(&self.initial_source()).map_err(|error| format!("{error:?}"))?;
        self.baseline = Some(parsed.ir);
        Ok(())
    }

    /// Re-anchors the baseline after a rejected or lost patch (for example a
    /// stale revision from the runtime): the projection's own tree is
    /// authoritative again at the runtime-reported revision.
    pub fn adopt_revision(&mut self, revision: u64) {
        self.revision = revision;
        let _ = self.adopt_baseline();
    }

    /// Diffs the current domain-derived tree against the last-emitted tree
    /// and advances the local baseline through the public `apply_ui_patch`
    /// contract, so local and runtime state stay lock-step. One-shot motion
    /// intents drained from the agent panel ride along as `StartTransition`
    /// ops; the IR applies them in the same revision bump as the diff.
    pub fn sync(&mut self) -> IdeProjectionUpdate {
        let Some(baseline) = self.baseline.clone() else {
            self.agent.take_pending_transitions();
            return IdeProjectionUpdate::FullSubmitRequired {
                source: self.initial_source(),
                reason: "baseline not adopted".into(),
            };
        };
        // Transitions may only target rows already present in the live
        // document: the IR resolves StartTransition before patch inserts,
        // and a motion aimed at a node that just vanished is stale intent.
        let transitions: Vec<UiPatchOp> = self
            .agent
            .take_pending_transitions()
            .into_iter()
            .filter(|(key, _)| tree_contains_key(&baseline.root, key))
            .map(|(key, transition)| UiPatchOp::StartTransition {
                node_path: format!("{TASK_LIST_SEMANTIC_PATH}/{key}"),
                transition,
            })
            .collect();
        let current = self.current_root();
        let diff = match diff_projection_trees(&baseline.root, &current) {
            Ok(diff) => diff,
            Err(reason) => {
                let (kind, detail) = match reason {
                    UiDiffRootMismatch::KeyChanged { old, new } => {
                        ("root_key_changed", format!("{old} -> {new}"))
                    }
                    UiDiffRootMismatch::IdentityChanged => ("root_identity_changed", String::new()),
                };
                return IdeProjectionUpdate::FullSubmitRequired {
                    source: self.initial_source(),
                    reason: format!("{kind}: {detail}"),
                };
            }
        };
        if diff.is_empty() {
            if transitions.is_empty() {
                return IdeProjectionUpdate::NoChange;
            }
            let patch = UiPatch {
                surface_id: self.surface_id.clone(),
                base_revision: self.revision,
                operations: transitions,
            };
            return match apply_ui_patch(&baseline, &patch) {
                Ok(applied) => {
                    self.baseline = Some(applied);
                    self.revision += 1;
                    IdeProjectionUpdate::Patch(patch)
                }
                Err(error) => IdeProjectionUpdate::FullSubmitRequired {
                    source: self.initial_source(),
                    reason: format!("local replay rejected the motion patch: {error:?}"),
                },
            };
        }
        let mut patch = build_ui_patch(
            &neon_ui_schema::UiSurfaceId(self.surface_id.clone()),
            self.revision,
            diff,
        );
        patch.operations.extend(transitions);
        match apply_ui_patch(&baseline, &patch) {
            Ok(applied) => {
                self.baseline = Some(applied);
                self.revision += 1;
                IdeProjectionUpdate::Patch(patch)
            }
            Err(error) => IdeProjectionUpdate::FullSubmitRequired {
                source: self.initial_source(),
                reason: format!("local replay rejected the diff: {error:?}"),
            },
        }
    }
}

/// Semantic path of the plan-list container that owns the task rows.
const TASK_LIST_SEMANTIC_PATH: &str = "workspace/agent/agent.section.tasks/agent.plan.list";

fn tree_contains_key(node: &UiNode, key: &str) -> bool {
    node.node_id.0 == key
        || node
            .children
            .iter()
            .any(|child| tree_contains_key(child, key))
}

fn write_flow_node(node: &UiNode, depth: usize, is_root: bool, out: &mut String) {
    let indent = "  ".repeat(depth);
    let mut line = if is_root {
        format!("surface {}", node.node_id.0)
    } else {
        match node.kind {
            UiNodeKind::Label => format!("text {}", node.node_id.0),
            UiNodeKind::Button => format!("button {}", node.node_id.0),
            _ => format!("panel {}", node.node_id.0),
        }
    };
    if let Some(layout) = &node.layout {
        match layout.mode {
            UiLayoutMode::Row => line.push_str(" row"),
            UiLayoutMode::Column => line.push_str(" column"),
            _ => {}
        }
    }
    line.push_str(&format!(
        " w {} h {}",
        format_number(node.bounds.width),
        format_number(node.bounds.height)
    ));
    line.push_str(&format!(
        " fill {} opacity {}",
        format_hex(node.style.background_color),
        format_number(node.style.opacity)
    ));
    if !node.visible {
        line.push_str(" visible false");
    }
    if !node.enabled {
        line.push_str(" enabled false");
    }
    if let Some(TextRef::Literal { value }) = &node.text {
        line.push_str(&format!(" value \"{}\"", escape_flow_string(value)));
    }
    out.push_str(&format!("{indent}{line}\n"));
    for child in &node.children {
        write_flow_node(child, depth + 1, false, out);
    }
}

pub fn summarize_patch_operations(patch: &UiPatch) -> Vec<String> {
    summarize_operations(&crate::ui_keyed_diff::UiTreeDiff {
        operations: patch.operations.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file(path: &str) -> FileTreeEntry {
        FileTreeEntry::file(path)
    }

    fn seeded_workspace() -> IdeWorkspaceProjection {
        let mut workspace = IdeWorkspaceProjection::new("surface.ide.test", 7);
        workspace.files.set_entries(vec![
            FileTreeEntry::dir("src"),
            sample_file("src/main.rs"),
            sample_file("src/util.rs"),
            FileTreeEntry::dir("docs"),
            sample_file("docs/readme.md"),
            sample_file("Cargo.toml"),
        ]);
        workspace.files.set_expanded("src", true);
        workspace.agent.add_task(AgentTask {
            plan: "alpha".into(),
            name: "build".into(),
            status: TaskStatus::Running,
            depends_on: None,
        });
        workspace.agent.record_transaction("t-1", "begin");
        workspace.adopt_baseline().expect("generated source parses");
        workspace
    }

    fn patch_of(update: IdeProjectionUpdate) -> UiPatch {
        match update {
            IdeProjectionUpdate::Patch(patch) => patch,
            other => panic!("expected a patch, got {other:?}"),
        }
    }

    fn only_sets(ops: &[String], expected: &[&str]) {
        assert_eq!(
            ops.iter().map(String::as_str).collect::<Vec<_>>(),
            expected,
            "unexpected op list"
        );
    }

    #[test]
    fn generated_source_parses_back_to_the_projection_tree() {
        let workspace = seeded_workspace();
        let parsed = parse_nui_flow(&workspace.initial_source())
            .expect("initial source must parse as canonical Flow");
        let diff = diff_projection_trees(&parsed.ir.root, &workspace.current_root())
            .expect("root identity must be stable");
        assert!(
            diff.is_empty(),
            "builder/parser parity broken: {:?}",
            summarize_operations(&diff)
        );
    }

    #[test]
    fn selection_switch_emits_property_sets_only() {
        let mut workspace = seeded_workspace();
        workspace.files.select(Some("src/main.rs".into()));
        let patch = patch_of(workspace.sync());
        only_sets(
            &summarize_patch_operations(&patch),
            &["set workspace/sidebar/row.src.main_drs.fill"],
        );
        assert_eq!(patch.base_revision, 7);
        workspace.files.select(Some("Cargo.toml".into()));
        let patch = patch_of(workspace.sync());
        only_sets(
            &summarize_patch_operations(&patch),
            &[
                "set workspace/sidebar/row.src.main_drs.fill",
                "set workspace/sidebar/row.Cargo_dtoml.fill",
            ],
        );
        assert_eq!(workspace.revision(), 9);
    }

    #[test]
    fn add_and_remove_file_are_single_structural_ops() {
        let mut workspace = seeded_workspace();
        let mut entries = workspace_file_entries();
        entries.insert(3, sample_file("src/lib.rs"));
        workspace.files.set_entries(entries);
        let patch = patch_of(workspace.sync());
        only_sets(
            &summarize_patch_operations(&patch),
            &["insert row.src.lib_drs@workspace/sidebar[3]"],
        );
        workspace.files.set_entries(workspace_file_entries());
        let patch = patch_of(workspace.sync());
        only_sets(
            &summarize_patch_operations(&patch),
            &["remove workspace/sidebar/row.src.lib_drs"],
        );
    }

    fn workspace_file_entries() -> Vec<FileTreeEntry> {
        vec![
            FileTreeEntry::dir("src"),
            FileTreeEntry::file("src/main.rs"),
            FileTreeEntry::file("src/util.rs"),
            FileTreeEntry::dir("docs"),
            FileTreeEntry::file("docs/readme.md"),
            FileTreeEntry::file("Cargo.toml"),
        ]
    }

    #[test]
    fn expand_and_collapse_are_contiguous_structural_ops() {
        let mut workspace = seeded_workspace();
        workspace.files.set_expanded("src", false);
        let patch = patch_of(workspace.sync());
        let summary = summarize_patch_operations(&patch);
        assert_eq!(
            summary,
            [
                "remove workspace/sidebar/row.src.main_drs",
                "remove workspace/sidebar/row.src.util_drs",
                "set workspace/sidebar/row.src.value",
            ]
        );
        workspace.files.set_expanded("src", true);
        let patch = patch_of(workspace.sync());
        let summary = summarize_patch_operations(&patch);
        assert_eq!(
            summary,
            [
                "insert row.src.main_drs@workspace/sidebar[1]",
                "insert row.src.util_drs@workspace/sidebar[2]",
                "set workspace/sidebar/row.src.value",
            ]
        );
    }

    #[test]
    fn rename_is_one_remove_and_one_insert_on_the_stable_key() {
        let mut workspace = seeded_workspace();
        let mut entries = workspace_file_entries();
        entries[1].path = "src/renamed.rs".into();
        workspace.files.set_entries(entries);
        let summary = summarize_patch_operations(&patch_of(workspace.sync()));
        assert_eq!(
            summary,
            [
                "remove workspace/sidebar/row.src.main_drs",
                "insert row.src.renamed_drs@workspace/sidebar[1]",
            ]
        );
    }

    #[test]
    fn busy_visual_state_is_a_property_set() {
        let mut workspace = seeded_workspace();
        workspace.files.set_busy("Cargo.toml", true);
        let patch = patch_of(workspace.sync());
        only_sets(
            &summarize_patch_operations(&patch),
            &["set workspace/sidebar/row.Cargo_dtoml.opacity"],
        );
    }

    #[test]
    fn unchanged_domain_state_emits_no_patch() {
        let mut workspace = seeded_workspace();
        workspace.files.select(Some("src/main.rs".into()));
        assert!(matches!(workspace.sync(), IdeProjectionUpdate::Patch(_)));
        assert!(matches!(workspace.sync(), IdeProjectionUpdate::NoChange));
    }

    #[test]
    fn task_status_change_touches_only_its_property() {
        let mut workspace = seeded_workspace();
        workspace
            .agent
            .set_task_status("alpha", "build", TaskStatus::Completed);
        let patch = patch_of(workspace.sync());
        let summary = summarize_patch_operations(&patch);
        only_sets(
            &summary,
            &[
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.fill",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.value",
                "transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build",
            ],
        );
        assert!(
            summary.iter().all(|entry| !entry.contains("sidebar")),
            "agent state must never touch the file tree: {summary:?}"
        );
    }

    #[test]
    fn completing_a_task_releases_its_dependent_with_one_set_and_one_insert() {
        let mut workspace = seeded_workspace();
        workspace.agent.add_task(AgentTask {
            plan: "alpha".into(),
            name: "deploy".into(),
            status: TaskStatus::Queued,
            depends_on: Some("build".into()),
        });
        // The dependent is gated behind `build`, so nothing changed yet.
        assert!(matches!(workspace.sync(), IdeProjectionUpdate::NoChange));
        workspace
            .agent
            .set_task_status("alpha", "build", TaskStatus::Completed);
        let summary = summarize_patch_operations(&patch_of(workspace.sync()));
        assert_eq!(
            summary,
            [
                "insert task.alpha.deploy@workspace/agent/agent.section.tasks/agent.plan.list[1]",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.fill",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.value",
                "transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build",
            ]
        );
    }

    #[test]
    fn add_and_remove_task_are_single_ops_scoped_to_the_agent_panel() {
        let mut workspace = seeded_workspace();
        workspace.agent.add_task(AgentTask {
            plan: "beta".into(),
            name: "package".into(),
            status: TaskStatus::Queued,
            depends_on: None,
        });
        let summary = summarize_patch_operations(&patch_of(workspace.sync()));
        assert_eq!(summary.len(), 1, "{summary:?}");
        assert!(summary[0].starts_with("insert "), "{summary:?}");
        workspace.agent.remove_task("beta", "package");
        let summary = summarize_patch_operations(&patch_of(workspace.sync()));
        assert_eq!(summary.len(), 1, "{summary:?}");
        assert!(summary[0].starts_with("remove "), "{summary:?}");
    }

    #[test]
    fn panel_switch_is_visibility_sets_inside_the_agent_panel_only() {
        let mut workspace = seeded_workspace();
        workspace
            .agent
            .set_section_visible(AgentSection::Records, false);
        let patch = patch_of(workspace.sync());
        let summary = summarize_patch_operations(&patch);
        only_sets(
            &summary,
            &["set workspace/agent/agent.section.records.visible"],
        );
    }

    #[test]
    fn approvals_and_changes_keep_stable_keys_when_resolved() {
        let mut workspace = seeded_workspace();
        workspace.agent.record_approval("a-1", "delete cache");
        let summary = summarize_patch_operations(&patch_of(workspace.sync()));
        assert_eq!(summary.len(), 1, "{summary:?}");
        assert!(summary[0].starts_with("insert "), "{summary:?}");
        workspace.agent.resolve_approval("a-1", true);
        let summary = summarize_patch_operations(&patch_of(workspace.sync()));
        only_sets(
            &summary,
            &["set workspace/agent/agent.section.records/agent.approvals/approval.a_h1.value"],
        );
    }

    #[test]
    fn sync_without_an_adopted_baseline_requires_a_full_submit() {
        let mut workspace = IdeWorkspaceProjection::new("surface.ide.test", 1);
        workspace.files.select(Some("a".into()));
        let update = workspace.sync();
        assert!(matches!(
            update,
            IdeProjectionUpdate::FullSubmitRequired { .. }
        ));
    }

    #[test]
    fn key_encoding_stays_injective_and_flow_valid() {
        assert_eq!(encode_key_segment("a_b"), "a__b");
        assert_eq!(encode_key_segment("a/b"), "a_x2fb");
        assert_eq!(encode_path("a/b.rs"), "a.b_drs");
        let mut seen = std::collections::BTreeSet::new();
        for path in [
            "a/b.rs",
            "a_b.rs",
            "a.b/rs",
            "a__b.rs",
            "src/main.rs",
            "src-main.rs",
        ] {
            assert!(
                seen.insert(format!("row.{}", encode_path(path))),
                "collision for {path}"
            );
        }
        for key in &seen {
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                "key {key} leaves the Flow vocabulary"
            );
        }
    }

    fn find_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
        if node.node_id.0 == key {
            return Some(node);
        }
        node.children.iter().find_map(|c| find_node(c, key))
    }

    fn ops_of(workspace: &mut IdeWorkspaceProjection) -> Vec<String> {
        summarize_patch_operations(&patch_of(workspace.sync()))
    }

    #[test]
    fn completed_task_rides_a_one_shot_success_sweep_in_the_same_patch() {
        let mut workspace = seeded_workspace();
        workspace
            .agent
            .set_task_status("alpha", "build", TaskStatus::Completed);
        let patch = patch_of(workspace.sync());
        assert_eq!(
            summarize_patch_operations(&patch),
            [
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.fill",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.value",
                "transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build",
            ]
        );
        assert_eq!(patch.base_revision, 7);
        assert_eq!(workspace.revision(), 8);
        let baseline = workspace.baseline.as_ref().expect("baseline advanced");
        assert_eq!(baseline.revision.0, 8);
        let item = find_node(&baseline.root, "task.alpha.build").expect("task row");
        let sweep = item.enter_transition.clone().expect("sweep landed");
        assert_eq!(sweep.duration_ms, 240);
        assert_eq!(sweep.motion_key.as_deref(), Some("success-sweep-1"));
        assert_eq!(sweep.from.opacity, Some(0.6));
    }

    #[test]
    fn failed_task_lands_text_action_row_and_flash_together() {
        let mut workspace = seeded_workspace();
        workspace
            .agent
            .set_task_status("alpha", "build", TaskStatus::Failed);
        let ops = ops_of(&mut workspace);
        assert_eq!(
            ops,
            [
                "insert task.alpha.build.retry@workspace/agent/agent.section.tasks/agent.plan.list[1]",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.fill",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.value",
                "transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build",
            ]
        );
        let root = workspace.current_root();
        let row = find_node(&root, "task.alpha.build").expect("row");
        let TextRef::Literal { value } = row.text.as_ref().expect("row text") else {
            panic!("task rows carry literal text")
        };
        assert_eq!(value, "alpha / build: failed", "failure must read as text");
        let retry = find_node(&root, "task.alpha.build.retry").expect("retry action exists");
        assert_eq!(retry.kind, UiNodeKind::Button);
        let TextRef::Literal { value } = retry.text.as_ref().expect("retry text") else {
            panic!("retry row carries literal text")
        };
        assert_eq!(value, "retry alpha / build");
    }

    #[test]
    fn retrying_clears_the_action_row_without_new_motion() {
        let mut workspace = seeded_workspace();
        workspace
            .agent
            .set_task_status("alpha", "build", TaskStatus::Failed);
        let _ = ops_of(&mut workspace);
        workspace.agent.retry_task("alpha", "build");
        let ops = ops_of(&mut workspace);
        assert_eq!(
            ops,
            [
                "remove workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.retry",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.fill",
                "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.value",
            ]
        );
        assert!(
            ops.iter().all(|op| !op.starts_with("transition")),
            "a retry press is a state change, not an animation"
        );
    }

    #[test]
    fn motions_only_fire_for_rows_present_in_the_live_document() {
        let mut workspace = seeded_workspace();
        workspace.agent.add_task(AgentTask {
            plan: "alpha".into(),
            name: "test".into(),
            status: TaskStatus::Queued,
            depends_on: Some("build".into()),
        });
        // The dependent has no row yet: its failure is domain state only.
        workspace
            .agent
            .set_task_status("alpha", "test", TaskStatus::Failed);
        assert_eq!(workspace.sync(), IdeProjectionUpdate::NoChange);
        assert!(workspace.agent.pending_transitions.is_empty());
    }

    #[test]
    fn a_motion_without_any_tree_change_still_emits_one_patch() {
        let mut workspace = seeded_workspace();
        workspace.agent.pending_transitions.push((
            "task.alpha.build".into(),
            UiTransition {
                delay_ms: 0,
                duration_ms: 150,
                easing: UiEasing::EaseOut,
                from: UiTransitionState {
                    opacity: Some(0.5),
                    ..Default::default()
                },
                motion_key: Some("blocked-tint-1".into()),
                timeline: None,
            },
        ));
        let patch = patch_of(workspace.sync());
        assert_eq!(
            summarize_patch_operations(&patch),
            ["transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build"]
        );
        assert_eq!(workspace.revision(), 8);
    }

    #[test]
    fn failed_task_source_round_trips_through_the_flow_parser() {
        let mut workspace = seeded_workspace();
        workspace
            .agent
            .set_task_status("alpha", "build", TaskStatus::Failed);
        let _ = ops_of(&mut workspace);
        let parsed =
            parse_nui_flow(&workspace.initial_source()).expect("button rows must round-trip");
        let diff = diff_projection_trees(&parsed.ir.root, &workspace.current_root())
            .expect("root identity stable");
        assert!(
            diff.is_empty(),
            "button parity broken: {:?}",
            summarize_operations(&diff)
        );
    }
}
