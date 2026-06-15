//! Ground-truth guards for the authoring-skill examples: every embedded
//! `ResourceExample` must match its real on-disk source byte-for-byte, and every
//! embedded example must validate clean against the live `KIND_REGISTRY`.

use cfgd_core::generate::validate::validate_document;
use cfgd_core::generate::{SkillKind, skill_model_for};

#[test]
fn module_example_matches_on_disk_source() {
    let model = skill_model_for(SkillKind::Module);
    let ex = model.examples.first().expect("module example present");
    let on_disk = std::fs::read_to_string(ex.source_path()).unwrap();
    assert_eq!(
        ex.contents.trim(),
        on_disk.trim(),
        "example drifted from its source file"
    );
}

#[test]
fn every_example_matches_its_on_disk_source() {
    for kind in [
        SkillKind::Module,
        SkillKind::Profile,
        SkillKind::Source,
        SkillKind::MachineConfig,
        SkillKind::ConfigPolicy,
        SkillKind::ClusterConfigPolicy,
    ] {
        let model = skill_model_for(kind);
        assert!(
            !model.examples.is_empty(),
            "{} has no ground-truth examples",
            kind.as_str()
        );
        for ex in &model.examples {
            let on_disk = std::fs::read_to_string(ex.source_path()).unwrap_or_else(|e| {
                panic!(
                    "{} example source {} unreadable: {e}",
                    kind.as_str(),
                    ex.source_path()
                )
            });
            assert_eq!(
                ex.contents.trim(),
                on_disk.trim(),
                "{} example drifted from its source file {}",
                kind.as_str(),
                ex.source_path()
            );
        }
    }
}

#[test]
fn every_embedded_example_validates_clean() {
    for kind in [
        SkillKind::Module,
        SkillKind::Profile,
        SkillKind::Source,
        SkillKind::MachineConfig,
        SkillKind::ConfigPolicy,
        SkillKind::ClusterConfigPolicy,
    ] {
        let model = skill_model_for(kind);
        for ex in &model.examples {
            let result = validate_document(&ex.contents);
            assert!(
                result.valid,
                "{} example {} failed validation: {:?}",
                kind.as_str(),
                ex.source_path(),
                result.errors
            );
        }
    }
}

#[test]
fn module_exemplar_carries_before_and_after() {
    let model = skill_model_for(SkillKind::Module);
    assert!(
        !model.exemplar.before.trim().is_empty(),
        "exemplar before is empty"
    );
    assert!(
        !model.exemplar.after.trim().is_empty(),
        "exemplar after is empty"
    );
    assert!(
        !model.exemplar.note.trim().is_empty(),
        "exemplar note is empty"
    );
    // The thorough rewrite is materially larger than the box-checking original.
    assert!(
        model.exemplar.after.lines().count() > model.exemplar.before.lines().count(),
        "exemplar after should be more thorough than before"
    );
    // Both halves are real Module manifests.
    assert!(model.exemplar.before.contains("kind: Module"));
    assert!(model.exemplar.after.contains("kind: Module"));
}
