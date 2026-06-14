//! Cross-crate guard: `cfgd_crd::is_valid_oci_reference` must agree with the
//! full `cfgd_core::oci::OciReference` parser on every input.
//!
//! `cfgd-crd` sits beneath `cfgd-core` in the dependency graph, so it cannot
//! call the real parser and instead reimplements the parser's reject rules as a
//! standalone predicate. This test — in the only crate that depends on both —
//! pins the two to the same accept/reject verdict so the predicate cannot drift
//! away from the parser undetected.

#[test]
fn predicate_agrees_with_parser_across_corpus() {
    let corpus = [
        "",
        "has space",
        "\u{7f}ctl",
        "localhost:5000",
        "foo:1234",
        "localhost:5000/repo",
        "foo:1234/bar",
        "registry.example.com/repo:v1",
        "ghcr.io/org/mod@sha256:abcd",
        "myrepo",
        "org/repo",
        "registry.example.com/",
    ];

    for s in corpus {
        let predicate = cfgd_crd::is_valid_oci_reference(s);
        let parser = cfgd_core::oci::OciReference::parse(s).is_ok();
        assert_eq!(
            predicate, parser,
            "cfgd_crd::is_valid_oci_reference({s:?}) = {predicate} but \
             cfgd_core::oci::OciReference::parse({s:?}).is_ok() = {parser} — \
             the predicate has drifted from the parser"
        );
    }
}
