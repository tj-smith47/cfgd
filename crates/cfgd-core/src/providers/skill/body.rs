//! The provider-agnostic skill body — the markdown each provider wraps in its
//! native envelope.
//!
//! [`render_skill_body`] composes the §7 thoroughness protocol (steps 0–6) from
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
/// The returned markdown carries the §7 protocol scaffold (precondition →
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

    // The field-walk command lines, pulled from the model so the explain token
    // is single-sourced.
    let explain_kind = format!("cfgd explain {token}");
    let explain_field = format!("cfgd explain {token}.<field>");

    let _ = writeln!(out, "## Protocol");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "0. **Precondition — confirm the toolchain is usable.** Run `command -v cfgd`; \
if it is absent, STOP and tell the user to install cfgd >= {min}. Run `cfgd --version`; \
if it is older than {min}, warn and prefer the embedded fallback schema below."
    );
    let _ = writeln!(
        out,
        "1. **Enumerate every field for this kind (live-first, snapshot-fallback).** \
Run `{explain_kind} -o json` for the authoritative live schema, and `{explain_field} -o json` \
to drill into nested objects. If cfgd is absent or older than the stamp, use the embedded \
fallback schema below (stamped {cfgd_version})."
    );
    let _ = writeln!(
        out,
        "2. **Research best practices externally for THIS subject.** {}",
        model.research_protocol
    );
    let _ = writeln!(
        out,
        "3. **For EVERY field, decide include OR omit, and justify with a WHY comment.** \
Box-checking is a failure; meeting the rubric above is the target."
    );
    let _ = writeln!(
        out,
        "4. **Draft thoroughly:** transitive deps explicit, version constraints set, \
platforms scoped, multi-step scripts idempotent (timeout + continueOnError), \
comments-as-specification."
    );
    let _ = writeln!(
        out,
        "5. **Validate against the schema:** `{}` — fix until clean \
(validate against the embedded snapshot if cfgd is unavailable).",
        model.validate_cmd
    );
    let _ = writeln!(
        out,
        "6. **Self-critique against the rubric:** \"Box-checking or thorough? Which field \
did I skip, and was that deliberate?\" Iterate until the answer holds."
    );
    let _ = writeln!(out);

    render_exemplar(&mut out, model);
    render_examples(&mut out, model);
    render_fallback_schema(&mut out, model);

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
    for example in &model.examples {
        write_fence(out, "yaml", &example.contents);
    }
}

/// Append the embedded fallback schema block under the pinned heading every
/// provider body carries. The heading and the ```json fence shape are a stable
/// contract providers depend on.
fn render_fallback_schema(out: &mut String, model: &SkillModel) {
    let _ = writeln!(out, "## Fallback schema (if cfgd is unavailable)");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Generated against cfgd {}. Live `cfgd explain {}` is authoritative when present.",
        model.schema_snapshot.cfgd_version, model.field_walk.explain_kind
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
        let model = skill_model_for(SkillKind::Module);
        let body = render_skill_body(&model);
        assert!(body.contains("cfgd explain module")); // step 1 field walk
        assert!(body.contains(&model.validate_cmd)); // step 5 validate
        assert!(body.contains("cfgd-min-version")); // runtime guard stamp
        assert!(body.contains("box-checking")); // thoroughness rubric
    }

    #[test]
    fn fallback_schema_block_is_present_and_fenced() {
        let model = skill_model_for(SkillKind::Module);
        let body = render_skill_body(&model);
        assert!(body.contains("## Fallback schema (if cfgd is unavailable)"));
        assert!(body.contains("```json"));
        assert!(body.contains(&model.schema_snapshot.json_schema));
    }

    #[test]
    fn version_stamp_carries_both_rendering_and_floor_versions() {
        let model = skill_model_for(SkillKind::Module);
        let body = render_skill_body(&model);
        let stamp = format!(
            "<!-- cfgd-version: {} · cfgd-min-version: {} -->",
            model.schema_snapshot.cfgd_version, model.min_cfgd_version
        );
        assert!(body.contains(&stamp), "missing exact stamp: {stamp}");
    }

    #[test]
    fn all_six_protocol_steps_are_present_in_order() {
        let model = skill_model_for(SkillKind::Profile);
        let body = render_skill_body(&model);
        let mut last = 0;
        for marker in [
            "0. **Precondition",
            "1. **Enumerate",
            "2. **Research",
            "3. **For EVERY field",
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
        let with = render_skill_body(&skill_model_for(SkillKind::Module));
        assert!(with.contains("## Worked exemplar (the quality bar)"));
        // Source has no exemplar, so the section is omitted entirely.
        let without = render_skill_body(&skill_model_for(SkillKind::Source));
        assert!(!without.contains("## Worked exemplar"));
    }
}
