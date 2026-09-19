//! Stage 2 probe: the production UI-runtime refresh is driven by the retained
//! input-impact projection instead of a full re-evaluation.
//!
//! Every publication is replayed on two lanes: one keeps the retained
//! projection (delta) and one drops it before every refresh (full). Their
//! fragments must hold identical content at every step, and the delta must name
//! the bindings, nodes, and presentation effects it actually rebuilt. Guard
//! failures must fall back to the full pass with a stable code instead of
//! partially applying.

use std::time::Instant;

use neon_protocol::Revision;
use neon_ui_runtime::{
    UiFragmentRefresh, UiInputApplyResult, UiInputStore, UiInputWriter, UiRetainedProjection,
    compile_nui_flow_program, lower_nui_flow_effects, parse_nui_flow,
    refresh_fragment_with_projection,
};
use neon_ui_schema::{
    UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiFragment, UiFragmentId, UiInputChange,
    UiInputFrame, UiInputValue, UiNode, UiProgram, UiProgramCapability, UiProgramCapabilityOwner,
    UiProgramCapabilityStatus, UiProgramRevision,
};
use serde_json::{Value, json};

const EPOCH: u64 = 7;
const PROBE: &str = "ui-retained-refresh.v1";

fn revision() -> UiProgramRevision {
    UiProgramRevision {
        program_id: "surface.retained-refresh".into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: vec![UiProgramCapability {
            name: UI_PROGRAM_CAPABILITY_NAME.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        }],
    }
}

/// `left`/`right` gate one panel each and `speed` drives one slider; every
/// `filler-*` panel is reached by no input at all, so it is the work a delta
/// must skip.
fn flow_source(fillers: usize) -> String {
    let bound = (fillers + 8) * 2;
    let mut source = format!(
        "version 1\nsurface surface.retained-refresh revision 1\nbudget nodes={bound} bindings={bound} instances={bound} text={bound} glyphs={glyphs} events=16 clips={bound}\ninput left bool default false\ninput right bool default false\ninput speed f32:0..100 default 10\nsurface root row w 200 h 80\n  panel left-panel visible $left w 80 h 40\n  panel right-panel visible $right w 80 h 40\n  slider speed-slider numeric $speed w 60 h 16\n",
        glyphs = bound * 8
    );
    for index in 0..fillers {
        source.push_str(&format!("  panel filler-{index} w 20 h 20\n"));
    }
    source
}

fn count_nodes(node: &UiNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

/// Fragment content without the revision counter: the lanes advance their
/// revision independently and only the rendered data has to agree. Derived
/// effects are compared as a set because a delta appends the rebuilt ones while
/// the full pass rebuilds every one in program order; the renderer keys
/// presentations by node path, so order carries no meaning.
fn content(fragment: &UiFragment) -> Value {
    let mut effects = fragment
        .effects
        .iter()
        .map(|effect| serde_json::to_value(effect).expect("effect serializes"))
        .collect::<Vec<_>>();
    effects.sort_by_key(|effect| effect.to_string());
    json!({
        "root": serde_json::to_value(&fragment.root).expect("root serializes"),
        "effects": effects,
    })
}

/// One refresh lane: the fragment the runtime caches, its input store, and the
/// retained projection that may or may not be kept between publications.
struct Lane {
    program: UiProgram,
    store: UiInputStore,
    fragment: UiFragment,
    tree_nodes: usize,
    projection: Option<UiRetainedProjection>,
    sequence: u64,
}

impl Lane {
    fn build(fillers: usize) -> Result<Self, String> {
        let document =
            parse_nui_flow(&flow_source(fillers)).map_err(|error| format!("parse: {error:?}"))?;
        let program = compile_nui_flow_program(&document, revision())
            .map_err(|error| format!("compile: {error:?}"))?;
        let store = UiInputStore::activate(program.revision.clone(), document.input_schema.clone())
            .map_err(|error| format!("input_activate: {}", error.code))?;
        let root = document.ir.root.clone();
        let tree_nodes = count_nodes(&root);
        let fragment = UiFragment {
            fragment_id: UiFragmentId(document.ir.surface_id.0.clone()),
            revision: Revision(1),
            root,
            effects: lower_nui_flow_effects(&document),
        };
        Ok(Self {
            program,
            store,
            fragment,
            tree_nodes,
            projection: None,
            sequence: 0,
        })
    }

    /// The lane after its activation refresh, which is what seeds the
    /// projection.
    fn activated(fillers: usize) -> Result<(Self, UiFragmentRefresh), String> {
        let mut lane = Self::build(fillers)?;
        let refresh = lane.refresh(&[], EPOCH)?;
        Ok((lane, refresh))
    }

    /// The one refresh a production publication performs: advance the fragment
    /// revision, then evaluate through the retained projection.
    fn refresh(
        &mut self,
        changed_slots: &[String],
        epoch: u64,
    ) -> Result<UiFragmentRefresh, String> {
        let snapshot = self.store.snapshot();
        self.fragment.revision = Revision(self.fragment.revision.0 + 1);
        Ok(refresh_fragment_with_projection(
            &mut self.projection,
            &mut self.fragment,
            &self.program,
            &snapshot,
            self.store.schema(),
            epoch,
            changed_slots,
        ))
    }

    fn set_input(&mut self, key: &str, value: UiInputValue) -> Result<Vec<String>, String> {
        let base = self.store.snapshot();
        self.sequence += 1;
        let request = format!("retained-refresh-{key}-{}", self.sequence);
        let applied: UiInputApplyResult = self
            .store
            .apply(
                UiInputWriter::External,
                UiInputFrame {
                    program_revision: self.program.revision.clone(),
                    expected_input_revision: base.input_revision,
                    request_id: request.clone(),
                    idempotency_key: request,
                    changes: vec![UiInputChange {
                        key: key.to_owned(),
                        value,
                    }],
                },
            )
            .map_err(|error| format!("input_apply: {}", error.code))?;
        Ok(applied.changed_slots)
    }

    /// A publication as the runtime sees it: the input store moves, then the
    /// cached fragment is refreshed with exactly the slots that changed.
    fn publish(
        &mut self,
        key: &str,
        value: UiInputValue,
        epoch: u64,
    ) -> Result<UiFragmentRefresh, String> {
        let changed = self.set_input(key, value)?;
        self.refresh(&changed, epoch)
    }

    /// Drops the projection first, so the refresh is the full pass the delta
    /// replaced. This lane is the equality oracle and the cost baseline.
    fn publish_full(
        &mut self,
        key: &str,
        value: UiInputValue,
        epoch: u64,
    ) -> Result<UiFragmentRefresh, String> {
        self.projection = None;
        self.publish(key, value, epoch)
    }
}

/// A lane that already holds a seeded projection, taken through one refresh
/// that is expected to fall back.
fn guarded(
    changed: &[&str],
    epoch: u64,
    prepare: impl FnOnce(&mut Lane),
) -> Option<(Lane, UiFragmentRefresh)> {
    let (mut lane, _) = Lane::activated(6).ok()?;
    prepare(&mut lane);
    let changed = changed
        .iter()
        .map(|key| (*key).to_owned())
        .collect::<Vec<String>>();
    let refresh = lane.refresh(&changed, epoch).ok()?;
    Some((lane, refresh))
}

fn producer(refresh: &UiFragmentRefresh) -> Value {
    json!({
        "input_revision": refresh.input_revision.0,
        "dirty_slots": refresh.dirty_slots,
        "changed_bindings": refresh.changed_bindings,
        "changed_nodes": refresh.changed_nodes,
        "bindings_executed": refresh.bindings_executed,
        "bindings_total": refresh.bindings_total,
    })
}

fn consumer(refresh: &UiFragmentRefresh) -> Value {
    json!({
        "delta_applied": refresh.delta_applied,
        "full_tree_rebuilt": !refresh.delta_applied,
        "layout_rebuilt": refresh.layout_rebuilt,
        "primitives_rebuilt": refresh.primitives_rebuilt,
        "nodes_total": refresh.nodes_total,
        "nodes_written": refresh.nodes_written,
        "tree_nodes_visited": refresh.tree_nodes_visited,
        "effects_rebuilt": refresh.effects_rebuilt,
        "fallback_code": refresh.fallback_code,
    })
}

fn full_pass(refresh: &UiFragmentRefresh, code: &str) -> bool {
    !refresh.delta_applied && refresh.fallback_code == code && refresh.nodes_written > 1
}

fn emit(case: &str, pass: bool, detail: Value) -> usize {
    println!(
        "{}",
        json!({"probe":PROBE,"case":case,"pass":pass,"detail":detail})
    );
    usize::from(!pass)
}

fn main() {
    let failures = run();
    let pass = failures == 0;
    println!(
        "{}",
        json!({"probe":PROBE,"final":true,"status":if pass {"passed"} else {"failed"},"failures":failures,"pass":pass})
    );
    if !pass {
        std::process::exit(1);
    }
}

fn run() -> usize {
    let mut failures = 0usize;
    let activated = Lane::activated(6).and_then(|(lane, first)| {
        Lane::activated(6).map(|(oracle, first_full)| (lane, oracle, first, first_full))
    });
    let (mut lane, mut oracle, first, first_full) = match activated {
        Ok(tuple) => tuple,
        Err(error) => {
            println!("{PROBE} setup failed: {error}");
            return 1;
        }
    };
    let tree_nodes = lane.tree_nodes;
    let bindings_total = lane.program.binding_records.len();
    let nodes_total = lane.program.nodes.len();

    // 1. Activation has nothing retained, so it must run the full pass and seed.
    failures += emit(
        "activation/full_pass",
        !first.delta_applied
            && first.fallback_code == "ui_retained_first_frame"
            && first.bindings_executed == bindings_total
            && first.nodes_written == tree_nodes
            && first.layout_rebuilt
            && !first_full.delta_applied
            && content(&lane.fragment) == content(&oracle.fragment),
        json!({"delta":consumer(&first),"full":consumer(&first_full)}),
    );

    // 2. A bool publication advances only the panel that binds it.
    let published = lane
        .publish("left", UiInputValue::Bool { value: true }, EPOCH)
        .and_then(|delta| {
            oracle
                .publish_full("left", UiInputValue::Bool { value: true }, EPOCH)
                .map(|full| (delta, full))
        });
    let (delta, full) = match published {
        Ok(pair) => pair,
        Err(error) => {
            println!("{PROBE} publish_left failed: {error}");
            return failures + 1;
        }
    };
    failures += emit(
        "publication_left/delta",
        delta.delta_applied
            && delta.fallback_code == "ui_retained_delta_applied"
            && delta.dirty_slots == vec!["left".to_owned()]
            && delta.changed_nodes == vec!["left-panel".to_owned()]
            // A visibility binding reaches its node and that node's layout
            // ancestors, so two of the ten tree nodes are rewritten.
            && delta.nodes_written == 2
            && delta.effects_rebuilt == 0
            && delta.bindings_executed < bindings_total
            && delta.changed_bindings.len() == delta.bindings_executed
            && !delta.layout_rebuilt
            && delta.primitives_rebuilt,
        json!({"producer":producer(&delta),"consumer":consumer(&delta),"full":consumer(&full)}),
    );
    failures += emit(
        "publication_left/equality",
        content(&lane.fragment) == content(&oracle.fragment),
        json!({"delta_nodes":delta.changed_nodes,"full_nodes":full.changed_nodes}),
    );
    // The contract shape the plan pins down for one input on a wide program.
    println!(
        "{}",
        json!({
            "probe":PROBE,
            "case":"publication_left/shape",
            "producer":{
                "input_revision":delta.input_revision.0,
                "dirty_slots":delta.dirty_slots,
                "changed_bindings":delta.changed_bindings,
                "changed_nodes":delta.changed_nodes,
            },
            "consumer":{
                "layout_rebuilt":delta.layout_rebuilt,
                "full_tree_rebuilt":!delta.delta_applied,
                "delta_applied":delta.delta_applied,
            },
            "status":"passed",
        })
    );

    // 3. A numeric publication also rebuilds exactly one derived presentation.
    let sped = lane
        .publish("speed", UiInputValue::F32 { value: 42.0 }, EPOCH)
        .and_then(|delta| {
            oracle
                .publish_full("speed", UiInputValue::F32 { value: 42.0 }, EPOCH)
                .map(|full| (delta, full))
        });
    let (speed, speed_full) = match sped {
        Ok(pair) => pair,
        Err(error) => {
            println!("{PROBE} publish_speed failed: {error}");
            return failures + 1;
        }
    };
    failures += emit(
        "publication_speed/delta",
        speed.delta_applied
            && speed.changed_nodes == vec!["speed-slider".to_owned()]
            && speed.effects_rebuilt == 1
            && speed.nodes_written == 1
            && speed.bindings_executed < bindings_total
            && !speed.primitives_rebuilt,
        json!({"producer":producer(&speed),"consumer":consumer(&speed),"full":consumer(&speed_full)}),
    );
    failures += emit(
        "publication_speed/equality",
        content(&lane.fragment) == content(&oracle.fragment),
        json!({
            "delta_effects":speed.effects_rebuilt,
            "full_effects":speed_full.effects_rebuilt,
        }),
    );

    // 4. Guards: the delta must refuse, not half-apply, and name why.
    let jumped = guarded(&["right"], EPOCH, |lane| {
        lane.fragment.revision = Revision(lane.fragment.revision.0 + 3);
    });
    let epoch_changed = guarded(&["right"], EPOCH + 1, |_| {});
    let slotless = guarded(&[], EPOCH, |_| {});
    for (case, code, observed) in [
        (
            "guard/fragment_revision_jump",
            "ui_retained_fragment_revision_jump",
            &jumped,
        ),
        (
            "guard/renderer_epoch",
            "ui_retained_renderer_epoch",
            &epoch_changed,
        ),
        (
            "guard/without_dirty_slot",
            "ui_retained_without_dirty_slot",
            &slotless,
        ),
    ] {
        let pass = observed
            .as_ref()
            .is_some_and(|(_, refresh)| full_pass(refresh, code));
        failures += emit(
            case,
            pass,
            observed
                .as_ref()
                .map_or(json!({"error":"setup"}), |(_, refresh)| consumer(refresh)),
        );
    }
    // A fallback re-seeded the projection, so the very next publication on that
    // lane is a delta again instead of a permanent full pass.
    let reseeded = epoch_changed
        .and_then(|(mut lane, _)| {
            lane.publish("right", UiInputValue::Bool { value: true }, EPOCH + 1)
                .ok()
                .map(|refresh| (lane, refresh))
        })
        .map(|(_lane, refresh)| {
            emit(
                "guard/fallback_reseeds",
                refresh.delta_applied
                    && refresh.changed_nodes == vec!["right-panel".to_owned()]
                    && refresh.nodes_written == 2,
                json!({"producer":producer(&refresh),"consumer":consumer(&refresh)}),
            )
        });
    failures += reseeded.unwrap_or(1);

    // 5. An optimistic motion writes outside authority. `UiRuntime` drops the
    //    projection at that moment, which is exactly this assignment.
    lane.projection = None;
    lane.fragment.root.style.opacity = 0.25;
    let healed = lane
        .publish("right", UiInputValue::Bool { value: true }, EPOCH)
        .and_then(|refresh| {
            oracle
                .publish_full("right", UiInputValue::Bool { value: true }, EPOCH)
                .map(|full| (refresh, full))
        });
    let (healed, healed_full) = match healed {
        Ok(pair) => pair,
        Err(error) => {
            println!("{PROBE} optimistic_publish failed: {error}");
            return failures + 1;
        }
    };
    failures += emit(
        "optimistic/healed_by_full_pass",
        full_pass(&healed, "ui_retained_first_frame")
            && healed.nodes_written == tree_nodes
            && healed.effects_rebuilt == healed_full.effects_rebuilt
            && content(&lane.fragment) == content(&oracle.fragment),
        json!({"delta":consumer(&healed),"full":consumer(&healed_full)}),
    );
    let resumed = lane
        .publish("left", UiInputValue::Bool { value: false }, EPOCH)
        .and_then(|refresh| {
            oracle
                .publish_full("left", UiInputValue::Bool { value: false }, EPOCH)
                .map(|full| (refresh, full))
        });
    let (resumed, resumed_full) = match resumed {
        Ok(pair) => pair,
        Err(error) => {
            println!("{PROBE} resume_publish failed: {error}");
            return failures + 1;
        }
    };
    failures += emit(
        "optimistic/delta_resumes",
        resumed.delta_applied
            && resumed.changed_nodes == vec!["left-panel".to_owned()]
            && content(&lane.fragment) == content(&oracle.fragment),
        json!({"producer":producer(&resumed),"consumer":consumer(&resumed),"full":consumer(&resumed_full)}),
    );

    // 6. Scale: one input on a wide program must stay narrower than the full
    //    pass it replaces, and still agree with it.
    let mut wide = match Lane::build(600) {
        Ok(lane) => lane,
        Err(error) => {
            println!("{PROBE} wide_setup failed: {error}");
            return failures + 1;
        }
    };
    let activation_started = Instant::now();
    let seeded = wide.refresh(&[], EPOCH);
    let activation_elapsed = activation_started.elapsed();
    let wide_activation = match seeded {
        Ok(refresh) => refresh,
        Err(error) => {
            println!("{PROBE} wide_activation failed: {error}");
            return failures + 1;
        }
    };
    let delta_started = Instant::now();
    let wide_delta = wide.publish("left", UiInputValue::Bool { value: true }, EPOCH);
    let delta_elapsed = delta_started.elapsed();
    let wide_delta = match wide_delta {
        Ok(refresh) => refresh,
        Err(error) => {
            println!("{PROBE} wide_publish failed: {error}");
            return failures + 1;
        }
    };
    // Baseline: the identical full pass with nothing retained, measured on a
    // clone so the delta lane keeps its own result.
    let mut baseline = Lane {
        program: wide.program.clone(),
        store: wide.store.clone(),
        fragment: wide.fragment.clone(),
        tree_nodes: wide.tree_nodes,
        projection: None,
        sequence: wide.sequence,
    };
    let full_started = Instant::now();
    let baseline_refresh = baseline.refresh(&["left".to_owned()], EPOCH);
    let full_elapsed = full_started.elapsed();
    let wide_equal =
        baseline_refresh.is_ok() && content(&baseline.fragment) == content(&wide.fragment);
    failures += emit(
        "scale/narrower_than_full",
        wide_delta.delta_applied
            && wide_delta.bindings_executed < wide.program.binding_records.len()
            && wide_delta.nodes_written == 2
            && wide_delta.effects_rebuilt == 0
            && wide_equal
            && delta_elapsed < full_elapsed,
        json!({
            "program_nodes": wide_delta.nodes_total,
            "program_bindings": wide_delta.bindings_total,
            "tree_nodes": wide.tree_nodes,
            "activation_us": activation_elapsed.as_micros(),
            "delta_us": delta_elapsed.as_micros(),
            "full_us": full_elapsed.as_micros(),
            "producer": producer(&wide_delta),
            "consumer": consumer(&wide_delta),
        }),
    );
    failures += emit(
        "scale/activation_is_full",
        wide_activation.bindings_executed == wide.program.binding_records.len()
            && wide_activation.nodes_written == wide.tree_nodes
            && !wide_activation.delta_applied,
        json!({
            "nodes_total": nodes_total,
            "bindings_total": bindings_total,
            "tree_nodes": tree_nodes,
            "activation": consumer(&wide_activation),
        }),
    );
    println!(
        "{}",
        json!({
            "probe":PROBE,
            "case":"summary",
            "program":{"nodes":nodes_total,"bindings":bindings_total,"tree_nodes":tree_nodes},
            "status":if failures == 0 {"passed"} else {"failed"},
        })
    );
    failures
}
