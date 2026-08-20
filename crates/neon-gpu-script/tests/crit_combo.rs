//! End-to-end tests for the crit_combo scene plus frontend error cases.

use neon_gpu_script::{
    compile, ConstValue, IrArg, KernelRegistry, NodeKind, ScriptError, WorldRegistry,
};

const CRIT_SRC: &str = r#"
# crit_combo.neoncomp — 每帧重跑：plan 缓存跨帧复用，只有输入变化
schema_version: 1

scene crit_combo = {
    input:
        target.stats as stats,
        target.def as def,
        frame.frame as frame,
        target.hp as target_hp
    output:
        target.hp
    body:
        let dmg = damage_formula(stats, def, kind="physical")
        let crit = rng_chance(seed=frame, chance=0.25)
        let hit = select(dmg, mul(dmg, 2.0), crit)
        let hp = apply_damage(target_hp, hit)
        export target.hp = hp
}
"#;

fn world() -> WorldRegistry {
    let mut w = WorldRegistry::new();
    w.register("target", "stats", "field<f32,[8]>", false);
    w.register("target", "def", "field<f32,[1]>", false);
    w.register("frame", "frame", "field<u32,[1]>", false);
    w.register("target", "hp", "field<f32,[1]>", true);
    w
}

fn kernels() -> KernelRegistry {
    let mut k = KernelRegistry::new();
    k.register("damage_formula", 2, &["kind"]);
    k.register("rng_chance", 0, &["seed", "chance"]);
    k.register("select", 3, &[]);
    k.register("mul", 2, &[]);
    k.register("apply_damage", 2, &[]);
    k
}

fn compile_ok(src: &str) -> neon_gpu_script::CompiledScript {
    compile(src, &world(), &kernels()).expect("script should compile")
}

fn compile_err(src: &str) -> ScriptError {
    compile(src, &world(), &kernels()).expect_err("script should fail")
}

#[test]
fn crit_combo_parses_and_validates() {
    let compiled = compile_ok(CRIT_SRC);
    assert_eq!(compiled.scenes.len(), 1);
    let scene = &compiled.scenes[0];
    assert_eq!(scene.ir.name, "crit_combo");
    assert_eq!(scene.ir.inputs.len(), 4);
    assert_eq!(scene.ir.outputs.len(), 1);
    assert_eq!(scene.ir.outputs[0].to_string(), "target.hp");
    assert_eq!(scene.ir.nodes.len(), 9, "4 input nodes + 5 kernel nodes (mul hoisted)");
    assert_eq!(scene.ir.exports.len(), 1);
}

#[test]
fn crit_combo_dag_edges_are_correct() {
    let scene = &compile_ok(CRIT_SRC).scenes[0];
    let nodes = &scene.ir.nodes;

    let idx = |name: &str| {
        nodes
            .iter()
            .position(|n| n.result == name)
            .unwrap_or_else(|| panic!("node {name} not found"))
    };
    let kernel_node = |kernel: &str| {
        nodes
            .iter()
            .position(|n| matches!(&n.kind, NodeKind::Kernel { kernel: k } if k == kernel))
            .unwrap_or_else(|| panic!("kernel {kernel} not found"))
    };

    let stats = idx("stats");
    let def = idx("def");
    let frame = idx("frame");
    let target_hp = idx("target_hp");
    let dmg = idx("dmg");
    let crit = idx("crit");
    let anon_mul = kernel_node("mul");
    let hit = idx("hit");
    let hp = idx("hp");

    assert!(matches!(&nodes[stats].kind, NodeKind::Input { .. }));
    assert_eq!(nodes[dmg].preds, vec![stats, def]);
    assert_eq!(nodes[crit].preds, vec![frame]);
    assert_eq!(
        nodes[hit].preds,
        vec![dmg, anon_mul, crit],
        "select(dmg, mul(dmg,..), crit): nested call hoisted to anonymous node"
    );
    assert_eq!(nodes[anon_mul].preds, vec![dmg]);
    assert!(nodes[anon_mul].result.starts_with('%'));
    assert_eq!(nodes[hp].preds, vec![target_hp, hit]);
}

#[test]
fn crit_combo_constants_are_baked() {
    let scene = &compile_ok(CRIT_SRC).scenes[0];
    let nodes = &scene.ir.nodes;
    let idx = |name: &str| nodes.iter().position(|n| n.result == name).unwrap();

    let dmg = &nodes[idx("dmg")];
    assert_eq!(
        dmg.args,
        vec![
            IrArg::Value(idx("stats")),
            IrArg::Value(idx("def")),
            IrArg::Const { key: "kind".into(), value: ConstValue::Str("physical".into()) },
        ]
    );

    let crit = &nodes[idx("crit")];
    assert_eq!(
        crit.args,
        vec![
            IrArg::Value(idx("frame")),
            IrArg::Const { key: "chance".into(), value: ConstValue::Number(0.25) },
        ]
    );
}

#[test]
fn crit_combo_waves_expose_parallelism() {
    let scene = &compile_ok(CRIT_SRC).scenes[0];
    let nodes = &scene.ir.nodes;
    let idx = |name: &str| nodes.iter().position(|n| n.result == name).unwrap();
    let kernel_node = |kernel: &str| {
        nodes
            .iter()
            .position(|n| matches!(&n.kind, NodeKind::Kernel { kernel: k } if k == kernel))
            .unwrap()
    };
    let dmg = idx("dmg");
    let crit = idx("crit");
    let anon_mul = kernel_node("mul");
    let hit = idx("hit");
    let hp = idx("hp");

    assert_eq!(
        scene.waves,
        vec![
            vec![dmg, crit],
            vec![anon_mul],
            vec![hit],
            vec![hp],
        ],
        "wave 0: dmg+crit parallel; wave 1: hoisted mul; wave 2: hit; wave 3: hp"
    );
}

#[test]
fn export_target_must_be_declared_output() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = mul(h, 2.0)
                export target.stats = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::UndeclaredOutput { ref domain, ref name } if domain == "target" && name == "stats"));
}

#[test]
fn two_writers_of_one_resource_conflict() {
    let err = compile_err(
        r#"
        scene a = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = mul(h, 2.0)
                export target.hp = h2
        }
        scene b = {
            input: target.hp as h
            output: target.hp
            body:
                let h3 = mul(h, 3.0)
                export target.hp = h3
        }
        "#,
    );
    assert!(matches!(err, ScriptError::WriterConflict { ref domain, ref name } if domain == "target" && name == "hp"));
}

#[test]
fn ssa_violation_on_double_assignment() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = mul(h, 2.0)
                let h2 = mul(h2, 2.0)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::SsaViolation { ref name } if name == "h2"));
}

#[test]
fn ssa_violation_when_let_shadows_input_alias() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h = mul(h, 2.0)
                export target.hp = h
        }
        "#,
    );
    assert!(matches!(err, ScriptError::SsaViolation { ref name } if name == "h"));
}

#[test]
fn undefined_value_reference_rejected() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = mul(h, missing)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::UndefinedValue { ref name } if name == "missing"));
}

#[test]
fn unknown_kernel_rejected() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = teleport(h)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::UnknownKernel { ref name } if name == "teleport"));
}

#[test]
fn unknown_world_resource_rejected() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.missing as m
            output: target.hp
            body:
                let h2 = mul(m, 2.0)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::UnknownWorld { ref domain, ref name } if domain == "target" && name == "missing"));
}

#[test]
fn read_only_resource_cannot_be_output() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.def as d
            output: target.def
            body:
                let d2 = mul(d, 2.0)
                export target.def = d2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::ReadOnlyOutput { ref domain, ref name } if domain == "target" && name == "def"));
}

#[test]
fn unknown_named_param_rejected() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = mul(h, 2.0, flavor=spicy)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::UnknownParam { ref name, ref param } if name == "mul" && param == "flavor"));
}

#[test]
fn wrong_value_arg_count_rejected() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                let h2 = mul(h)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::KernelArgCount { ref name, expected, actual } if name == "mul" && expected == 2 && actual == 1));
}

#[test]
fn qualified_names_are_two_part_only() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp.extra as h
            output: target.hp
            body:
                let h2 = mul(h, 2.0)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::Parse { .. }));
}

#[test]
fn lex_error_on_illegal_character() {
    let err = compile_err("scene s = { input: target.hp as h $ }");
    assert!(matches!(err, ScriptError::Lex { .. }));
}

#[test]
fn empty_input_list_is_valid_but_useless_scene_rejected_if_export_undefined() {
    let err = compile_err(
        r#"
        scene s = {
            input: target.hp as h
            output: target.hp
            body:
                export target.hp = nowhere
        }
        "#,
    );
    assert!(matches!(err, ScriptError::UndefinedValue { ref name } if name == "nowhere"));
}

#[test]
fn duplicate_input_alias_rejected() {
    let err = compile_err(
        r#"
        scene s = {
            input:
                target.hp as h,
                target.def as h
            output: target.hp
            body:
                let h2 = mul(h, 2.0)
                export target.hp = h2
        }
        "#,
    );
    assert!(matches!(err, ScriptError::SsaViolation { ref name } if name == "h"));
}