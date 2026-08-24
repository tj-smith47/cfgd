//! The provider-agnostic skill body — the markdown each provider wraps in its
//! native envelope.
//!
//! [`render_skill_body`] composes the thoroughness protocol (steps 0–6) from
//! a [`SkillModel`]'s already-structured fields: the version stamp and runtime
//! guard, the field-walk and validate commands, the rubric and research loop,
//! the embedded fallback schema, the worked exemplar, and the ground-truth
//! examples. The rubric/research/validate/explain text is pulled verbatim from
//! the model so this renderer is the single composition site, not a second
//! authoring site for the doctrine.

use std::fmt::Write;

use crate::generate::SkillModel;

/// Render the provider-agnostic skill body for `model`.
///
/// The returned markdown carries the protocol scaffold (precondition →
/// enumerate → research → decide+justify → draft → validate → self-critique),
/// the body-level `<!-- cfgd-version: … · cfgd-min-version: … -->` stamp read by
/// step 0, a fenced `## Fallback schema (if cfgd is unavailable)` block, the
/// before/after exemplar, and the captured ground-truth examples. Providers wrap
/// this verbatim in their native envelope (frontmatter, TOML, managed block).
pub fn render_skill_body(model: &SkillModel) -> String {
    let kind_word = model.kind.as_str();
    let token = model.field_walk.explain_kind;
    let min = &model.min_cfgd_version;
    let cfgd_version = &model.schema_snapshot.cfgd_version;

    let mut out = String::new();

    // Body-level version stamp, read by protocol step 0. Providers with native
    // frontmatter additionally surface these keys there; that duplication is by
    // design (frontmatter for tooling, comment for the agent).
    let _ = writeln!(
        out,
        "<!-- cfgd-version: {cfgd_version} · cfgd-min-version: {min} -->"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "# Author a high-quality cfgd {kind_word}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Follow this protocol on every invocation. {}",
        model.thoroughness_rubric
    );
    let _ = writeln!(out);

    // The field-walk command line, pulled from the model so the explain token
    // is single-sourced. The drill-down clause is gated on `drill_hint` per the
    // FieldWalkSpec contract. `-o json` already returns the whole nested tree
    // (every field's `children`), so the drill-down is the HUMAN form for
    // reading one field's docs, never a second JSON walk.
    let explain_kind = format!("cfgd explain {token}");
    let drill_clause = if model.field_walk.drill_hint {
        format!(" `{explain_kind}.<field>` (no `-o`) prints one field's docs readably.")
    } else {
        String::new()
    };

    let _ = writeln!(out, "## Protocol");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "0. **Precondition.** Run `cfgd --version`. If cfgd is absent, STOP and tell the \
user to install cfgd >= {min}; if it is older than {min}, warn and take the fallback branch \
in steps 1 and 5."
    );
    let _ = writeln!(
        out,
        "1. **Enumerate every field.** Run `{explain_kind} -o json` once. The payload is the \
complete field list step 3 walks: every field, nested ones under `children`, each with \
`type`, `description` and `required`; its `location` is the path the finished file goes \
to.{drill_clause} Fallback: the embedded schema below (stamped {cfgd_version})."
    );
    let _ = writeln!(
        out,
        "2. **Research THIS subject before choosing values.** {}",
        model.research_protocol
    );
    let _ = writeln!(
        out,
        "3. **Decide include or omit for EVERY field from step 1, and write the WHY as a \
comment beside each included one.** Omit a field the subject does not use or whose value \
would equal the default; note a non-obvious omission in a comment too."
    );
    let _ = writeln!(
        out,
        "4. **Draft.** Declare every dependency the subject needs at run time, transitive \
ones included. Set a version floor only where a feature needs it, and say which. Gate \
platform-specific entries with `platforms`. Make each script step safe to re-run (`onlyIf` \
/ `unless` / `creates` where the kind offers them, or a command that is itself idempotent), \
give it a `timeout`, and set `continueOnError: true` only where a failure must not abort \
the apply. Never write a credential into a value; a secret belongs in the profile's \
`spec.secrets`. No placeholders, no stub comments."
    );
    let _ = writeln!(
        out,
        "5. **Validate:** `{}` (`-` reads stdin; add `-o json` for a parseable report). A \
non-zero exit lists every error with its line; fix and re-run until it prints `✓ … is \
valid`. Fallback: check the draft by hand against the embedded schema (required keys, \
types, enums) and tell the user it was not machine-validated.",
        model.validate_cmd
    );
    let _ = writeln!(
        out,
        "6. **Self-critique.** For each field in the step-1 list, name the evidence behind \
its value or its omission; a field you cannot account for goes back to step 2."
    );
    let _ = writeln!(out);

    render_exemplar(&mut out, model);
    render_examples(&mut out, model);
    render_fallback_schema(&mut out, model, &explain_kind);

    out
}

/// Append the before/after worked exemplar when the kind ships one. The default
/// (empty) exemplar is skipped so kinds without one carry no empty section.
fn render_exemplar(out: &mut String, model: &SkillModel) {
    let ex = &model.exemplar;
    if ex.before.is_empty() && ex.after.is_empty() && ex.note.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Worked exemplar (the quality bar)");
    let _ = writeln!(out);
    if !ex.note.is_empty() {
        let _ = writeln!(out, "{}", ex.note);
        let _ = writeln!(out);
    }
    if !ex.before.is_empty() {
        let _ = writeln!(out, "Before (box-checking):");
        let _ = writeln!(out);
        write_fence(out, "yaml", &ex.before);
    }
    if !ex.after.is_empty() {
        let _ = writeln!(out, "After (thorough):");
        let _ = writeln!(out);
        write_fence(out, "yaml", &ex.after);
    }
}

/// Append the captured ground-truth example(s) for the kind, each in a fenced
/// YAML block.
fn render_examples(out: &mut String, model: &SkillModel) {
    if model.examples.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Ground-truth examples");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Validated resources of this kind, shown for shape and depth. A value like \
`you@example.com` is the example's placeholder; your draft carries the real one."
    );
    let _ = writeln!(out);
    for example in &model.examples {
        write_fence(out, "yaml", &example.contents);
    }
}

/// Append the embedded fallback schema block under the pinned heading every
/// provider body carries. The heading and the ```json fence shape are a stable
/// contract providers depend on. `explain_kind` is the same `cfgd explain <kind>`
/// command spelling used by protocol step 1, threaded through so the explain
/// command has one source.
fn render_fallback_schema(out: &mut String, model: &SkillModel, explain_kind: &str) {
    let _ = writeln!(out, "## Fallback schema (if cfgd is unavailable)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Generated against cfgd {}. Live `{explain_kind}` is authoritative when present.",
        model.schema_snapshot.cfgd_version
    );
    let _ = writeln!(out);
    write_fence(out, "json", &model.schema_snapshot.json_schema);
}

/// Write a fenced code block for `body` under `lang`, guaranteeing the closing
/// fence sits on its own line regardless of `body`'s trailing newline.
fn write_fence(out: &mut String, lang: &str, body: &str) {
    let _ = writeln!(out, "```{lang}");
    let _ = writeln!(out, "{}", body.trim_end_matches('\n'));
    let _ = writeln!(out, "```");
    let _ = writeln!(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{SkillKind, skill_model_for};

    #[test]
    fn skill_body_contains_protocol_validate_and_version_stamp() {
        let model = skill_model_for(SkillKind::Module, env!("CARGO_PKG_VERSION"));
        let body = render_skill_body(&model);
        assert!(body.contains("cfgd explain module")); // step 1 field walk
        assert!(body.contains(&model.validate_cmd)); // step 5 validate
        assert!(body.contains("cfgd-min-version")); // runtime guard stamp
        assert!(body.contains("box-checking")); // thoroughness rubric
    }

    #[test]
    fn fallback_schema_block_is_present_and_fenced() {
        let model = skill_model_for(SkillKind::Module, env!("CARGO_PKG_VERSION"));
        let body = render_skill_body(&model);
        assert!(body.contains("## Fallback schema (if cfgd is unavailable)"));
        assert!(body.contains("```json"));
        assert!(body.contains(&model.schema_snapshot.json_schema));
    }

    #[test]
    fn version_stamp_carries_both_rendering_and_floor_versions() {
        let model = skill_model_for(SkillKind::Module, env!("CARGO_PKG_VERSION"));
        let body = render_skill_body(&model);
        // Assemble the expected stamp from literal fragments — with the real
        // middot baked into the literal — rather than the renderer's own format
        // string, so a middot->hyphen (or any separator) drift fails the test
        // instead of passing silently.
        let stamp = String::new()
            + "<!-- cfgd-version: "
            + &model.schema_snapshot.cfgd_version
            + " · cfgd-min-version: "
            + &model.min_cfgd_version.to_string()
            + " -->";
        assert!(body.contains(&stamp), "missing exact stamp: {stamp}");
    }

    #[test]
    fn all_six_protocol_steps_are_present_in_order() {
        let model = skill_model_for(SkillKind::Profile, env!("CARGO_PKG_VERSION"));
        let body = render_skill_body(&model);
        let mut last = 0;
        for marker in [
            "0. **Precondition",
            "1. **Enumerate",
            "2. **Research",
            "3. **Decide",
            "4. **Draft",
            "5. **Validate",
            "6. **Self-critique",
        ] {
            let at = body
                .find(marker)
                .unwrap_or_else(|| panic!("step marker absent: {marker}"));
            assert!(at >= last, "step out of order: {marker}");
            last = at;
        }
    }

    #[test]
    fn exemplar_rendered_only_when_present() {
        let with = render_skill_body(&skill_model_for(
            SkillKind::Module,
            env!("CARGO_PKG_VERSION"),
        ));
        assert!(with.contains("## Worked exemplar (the quality bar)"));
        // Source has no exemplar, so the section is omitted entirely.
        let without = render_skill_body(&skill_model_for(
            SkillKind::Source,
            env!("CARGO_PKG_VERSION"),
        ));
        assert!(!without.contains("## Worked exemplar"));
    }
}
